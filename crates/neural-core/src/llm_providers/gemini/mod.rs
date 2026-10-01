use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::adapter::LlmTransportKind;
use crate::llm_providers::capabilities::gemini_provider_capabilities;
use crate::llm_providers::cli_output::json_field_text;
use crate::llm_providers::cli_stream::CliJsonTextStreamState;
use crate::llm_providers::command_runner::{
    ProviderCommandRunner, cli_spawn, model_args, prompt_text, replace_arg_value, response,
    selected_model,
};
use crate::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmProvider, LlmStreamEventSink, ProviderCallControl,
};
use crate::llm_providers::gemini::api::GeminiDirectApiTransport;
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::{
    LlmModalityInput, LlmModalityInputKind, LlmProviderCapabilities, LlmRequest, LlmResponse,
    LlmStreamEvent,
};
use crate::process::StreamControl;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub mod api;
pub mod live;

use live::{
    GeminiLiveCredential, GeminiLiveInput, GeminiLiveTransport, GeminiLiveTurn,
    GeminiLiveWebSocketTransport,
};

const DEFAULT_GEMINI_LIVE_MODEL: &str = "gemini-3.1-flash-live-preview";
static GEMINI_MCP_WORKSPACE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct GeminiProvider {
    config: LlmProviderConfig,
    runner: Option<ProviderCommandRunner>,
    live_transport: Arc<dyn GeminiLiveTransport>,
    direct_api: Option<GeminiDirectApiTransport>,
    selected_transport: LlmTransportKind,
}

impl GeminiProvider {
    pub fn new(config: LlmProviderConfig) -> Result<Self> {
        Self::new_with_transport(
            config,
            Arc::new(NoGeminiHttpClient),
            LlmTransportKind::Client,
        )
    }

    pub fn new_with_transport(
        config: LlmProviderConfig,
        http_client: Arc<dyn LlmHttpClient>,
        selected_transport: LlmTransportKind,
    ) -> Result<Self> {
        config.validate()?;
        let runner = match selected_transport {
            LlmTransportKind::Client => Some(ProviderCommandRunner::new(cli_spawn(&config)?)?),
            _ => None,
        };
        let endpoint = config.endpoint.clone();
        let direct_api = match selected_transport {
            LlmTransportKind::DirectApi => {
                Some(GeminiDirectApiTransport::new(config.clone(), http_client))
            }
            _ => None,
        };
        Ok(Self {
            config,
            runner,
            live_transport: Arc::new(GeminiLiveWebSocketTransport::new(endpoint)),
            direct_api,
            selected_transport,
        })
    }

    pub fn new_with_live_transport(
        config: LlmProviderConfig,
        live_transport: Arc<dyn GeminiLiveTransport>,
    ) -> Result<Self> {
        config.validate()?;
        let runner = ProviderCommandRunner::new(cli_spawn(&config)?)?;
        Ok(Self {
            config,
            runner: Some(runner),
            live_transport,
            direct_api: None,
            selected_transport: LlmTransportKind::Client,
        })
    }
}

impl LlmProvider for GeminiProvider {
    fn run_audio_session(
        &self,
        request: crate::llm_providers::AudioSessionRequest,
        input: crate::llm_providers::AudioSessionInput,
        sink: &mut crate::llm_providers::AudioSessionEventSink<'_>,
    ) -> Result<()> {
        crate::llm_providers::realtime::audio_connection::run_gemini(
            &self.config,
            request,
            input,
            sink,
        )
    }

    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        gemini_provider_capabilities(&self.config.provider_id)
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        if let Some(direct_api) = &self.direct_api {
            return direct_api.complete(request);
        }
        validate_llm_request(request)?;
        validate_gemini_complete_modality_inputs(&self.config, request)?;
        let workspace = GeminiMcpWorkspace::new(request)?;
        let output = self.client_runner()?.run_with_cwd(
            gemini_args(&self.config, request),
            None,
            workspace.path(),
        )?;
        let content = json_field_text("Gemini", &output.stdout, "response")?;
        let session_id = self.gemini_session_id(request, workspace.path());
        Ok(response(
            &self.config,
            request,
            content,
            gemini_metadata(session_id),
        ))
    }

    fn complete_controlled(
        &self,
        request: &LlmRequest,
        control: &dyn ProviderCallControl,
    ) -> Result<LlmResponse> {
        if control.is_cancelled() {
            return Err(NeuralError::ProcessCancelled {
                command: self.provider_id().into(),
            });
        }
        if control.is_expired() {
            return Err(NeuralError::ProcessTimeout {
                command: self.provider_id().into(),
                timeout_ms: 0,
            });
        }
        if let Some(direct_api) = &self.direct_api {
            return direct_api.complete_with_control(
                request,
                StreamControl::unbounded().with_deadline(Instant::now() + control.remaining()),
            );
        }
        validate_llm_request(request)?;
        validate_gemini_complete_modality_inputs(&self.config, request)?;
        let workspace = GeminiMcpWorkspace::new(request)?;
        let output = self.client_runner()?.run_with_cwd_cancellable(
            gemini_args(&self.config, request),
            None,
            workspace.path(),
            control,
        )?;
        let content = json_field_text("Gemini", &output.stdout, "response")?;
        let session_id = self.gemini_session_id(request, workspace.path());
        Ok(response(
            &self.config,
            request,
            content,
            gemini_metadata(session_id),
        ))
    }

    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        if let Some(direct_api) = &self.direct_api {
            return direct_api.stream_with_events(request, control, on_event);
        }
        validate_llm_request(request)?;
        validate_gemini_stream_modality_inputs(&self.config, request)?;
        if request_requires_live_transport(request) {
            let turn = gemini_live_turn(&self.config, request)?;
            return self.live_transport.stream_turn(turn, control, on_event);
        }
        let mut state = CliJsonTextStreamState::new(control);
        let workspace = GeminiMcpWorkspace::new(request)?;
        self.client_runner()?.run_stdout_lines_with_cwd(
            gemini_stream_args(&self.config, request),
            None,
            workspace.path(),
            &mut |line| state.push_line("Gemini", &line, on_event),
        )?;
        if state.cancelled() {
            return on_event(LlmStreamEvent::Cancelled);
        }
        if let Some(session_id) = self.gemini_session_id(request, workspace.path()) {
            on_event(LlmStreamEvent::Session {
                provider_session_id: session_id,
            })?;
        }
        on_event(LlmStreamEvent::Complete)
    }
}

impl GeminiProvider {
    fn gemini_session_id(&self, request: &LlmRequest, cwd: Option<&Path>) -> Option<String> {
        if let Some(session_id) = cleaned_session_id(request.provider_session_id.as_deref()) {
            return Some(session_id);
        }
        request.conversation_id.as_ref()?;
        self.client_runner()
            .ok()
            .and_then(|runner| {
                runner
                    .run_with_cwd(gemini_list_session_args(), None, cwd)
                    .ok()
            })
            .and_then(|output| gemini_latest_session_id(&output.stdout))
    }

    fn client_runner(&self) -> Result<&ProviderCommandRunner> {
        self.runner
            .as_ref()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: format!("{:?}", self.selected_transport),
                expected: "Gemini client transport".to_string(),
            })
    }
}

struct NoGeminiHttpClient;

impl LlmHttpClient for NoGeminiHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<serde_json::Value> {
        Err(NeuralError::MissingValue {
            value: request.endpoint.clone(),
            expected: "configured Gemini HTTP client".to_string(),
        })
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        Err(NeuralError::MissingValue {
            value: request.endpoint.clone(),
            expected: "configured Gemini HTTP client".to_string(),
        })
    }
}

fn gemini_args(config: &LlmProviderConfig, request: &LlmRequest) -> Vec<String> {
    let mut args = model_args(&selected_model(config, request));
    args.extend([
        "--prompt".to_string(),
        prompt_text(request),
        "--output-format".to_string(),
        "json".to_string(),
        "--approval-mode".to_string(),
        gemini_approval_mode(request).to_string(),
        "--sandbox".to_string(),
    ]);
    args.extend(gemini_resume_args(request));
    args.extend(gemini_mcp_args(request));
    args
}

fn gemini_resume_args(request: &LlmRequest) -> Vec<String> {
    cleaned_session_id(request.provider_session_id.as_deref())
        .map(|session_id| vec!["--resume".to_string(), session_id])
        .unwrap_or_default()
}

fn gemini_list_session_args() -> Vec<String> {
    vec!["--list-sessions".to_string()]
}

fn gemini_metadata(provider_session_id: Option<String>) -> serde_json::Value {
    let mut metadata = json!({ "transport": "client" });
    if let (Some(object), Some(session_id)) = (metadata.as_object_mut(), provider_session_id) {
        object.insert("provider_session_id".to_string(), json!(session_id));
    }
    metadata
}

fn cleaned_session_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn gemini_latest_session_id(value: &str) -> Option<String> {
    value.lines().find_map(gemini_session_id_from_line)
}

fn gemini_session_id_from_line(line: &str) -> Option<String> {
    let start = line.rfind('[')?;
    let end = line.rfind(']')?;
    if end <= start {
        return None;
    }
    cleaned_session_id(Some(&line[start + 1..end]))
}

fn gemini_approval_mode(request: &LlmRequest) -> &'static str {
    if request.mcp_servers.is_empty() {
        return "plan";
    }
    // `-p` has no interactive approver. Scoped MCP tools are first-party and
    // already limited to the request's server names, so every tool call must
    // be approved without prompting.
    "yolo"
}

fn gemini_mcp_args(request: &LlmRequest) -> Vec<String> {
    if request.mcp_servers.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["--allowed-mcp-server-names".to_string()];
    args.extend(request.mcp_servers.iter().map(|server| server.name.clone()));
    args
}

fn gemini_stream_args(config: &LlmProviderConfig, request: &LlmRequest) -> Vec<String> {
    let mut args = gemini_args(config, request);
    replace_arg_value(&mut args, "--output-format", "stream-json");
    args
}

struct GeminiMcpWorkspace {
    path: Option<PathBuf>,
}

impl GeminiMcpWorkspace {
    fn new(request: &LlmRequest) -> Result<Self> {
        if request.mcp_servers.is_empty() {
            return Ok(Self { path: None });
        }
        let path = gemini_mcp_workspace_path();
        let settings_dir = path.join(".gemini");
        std::fs::create_dir_all(&settings_dir).map_err(|source| NeuralError::Io {
            value: settings_dir.display().to_string(),
            expected: "Gemini MCP settings directory".to_string(),
            source,
        })?;
        write_gemini_mcp_settings(&settings_dir.join("settings.json"), request)?;
        Ok(Self { path: Some(path) })
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl Drop for GeminiMcpWorkspace {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn gemini_mcp_workspace_path() -> PathBuf {
    let sequence = GEMINI_MCP_WORKSPACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "lumvise-gemini-mcp-{}-{sequence}",
        std::process::id()
    ))
}

fn write_gemini_mcp_settings(path: &Path, request: &LlmRequest) -> Result<()> {
    let settings = serde_json::json!({
        "mcp": { "allowed": gemini_mcp_server_names(request) },
        "mcpServers": gemini_mcp_server_settings(request),
    });
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|source| NeuralError::Json {
        value: path.display().to_string(),
        expected: "Gemini MCP settings JSON".to_string(),
        source,
    })?;
    std::fs::write(path, bytes).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: "Gemini MCP settings file".to_string(),
        source,
    })
}

fn gemini_mcp_server_names(request: &LlmRequest) -> Vec<String> {
    request
        .mcp_servers
        .iter()
        .map(|server| server.name.clone())
        .collect()
}

fn gemini_mcp_server_settings(request: &LlmRequest) -> serde_json::Map<String, serde_json::Value> {
    request
        .mcp_servers
        .iter()
        .map(|server| {
            (
                server.name.clone(),
                serde_json::json!({ "url": server.url, "trust": true }),
            )
        })
        .collect()
}

fn request_requires_live_transport(request: &LlmRequest) -> bool {
    request.modality_inputs.iter().any(|input| {
        matches!(
            input.kind,
            LlmModalityInputKind::LiveAudioChunk | LlmModalityInputKind::ScreenFrame
        )
    })
}

fn validate_gemini_complete_modality_inputs(
    config: &LlmProviderConfig,
    request: &LlmRequest,
) -> Result<()> {
    if let Some(input) = request.modality_inputs.first() {
        return Err(unsupported_gemini_modality(
            config,
            input,
            "text-only Gemini complete request; use streaming Gemini Live for live modalities",
        ));
    }
    Ok(())
}

fn validate_gemini_stream_modality_inputs(
    config: &LlmProviderConfig,
    request: &LlmRequest,
) -> Result<()> {
    for input in &request.modality_inputs {
        if input.kind == LlmModalityInputKind::ImageSnapshot {
            return Err(unsupported_gemini_modality(
                config,
                input,
                "Gemini live_audio_input or screen_frame_broadcast_input; image_snapshot_input is unsupported",
            ));
        }
    }
    Ok(())
}

fn unsupported_gemini_modality(
    config: &LlmProviderConfig,
    input: &LlmModalityInput,
    expected: &str,
) -> NeuralError {
    NeuralError::InvalidValue {
        value: format!("{}:{}", config.provider_id, input.input_id),
        expected: expected.to_string(),
    }
}

pub(crate) fn gemini_live_turn(
    config: &LlmProviderConfig,
    request: &LlmRequest,
) -> Result<GeminiLiveTurn> {
    Ok(GeminiLiveTurn {
        provider_id: config.provider_id.clone(),
        model: selected_gemini_live_model(config, request)?,
        credential: gemini_live_credential(config)?,
        session_resumption_handle: request.provider_session_id.clone(),
        user_text: prompt_text(request),
        mcp_servers: request.mcp_servers.clone(),
        inputs: gemini_live_inputs(request),
    })
}

fn selected_gemini_live_model(config: &LlmProviderConfig, request: &LlmRequest) -> Result<String> {
    let model = selected_model(config, request);
    if gemini_model_supports_live(&model) {
        return Ok(model);
    }
    if request.model_id().is_none() {
        return Ok(DEFAULT_GEMINI_LIVE_MODEL.to_string());
    }
    Err(NeuralError::InvalidValue {
        value: model,
        expected: format!("Gemini Live API model such as `{DEFAULT_GEMINI_LIVE_MODEL}`"),
    })
}

fn gemini_model_supports_live(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized.contains("-live") || normalized.contains("native-audio")
}

fn gemini_live_credential(config: &LlmProviderConfig) -> Result<GeminiLiveCredential> {
    let credential = config.credential.as_deref().unwrap_or_default().trim();
    if credential.is_empty() {
        return Err(NeuralError::MissingValue {
            value: "gemini credential".to_string(),
            expected:
                "non-empty Gemini API key, OAuth bearer token, or ephemeral token for Live API websocket requests"
                    .to_string(),
        });
    }
    Ok(parse_gemini_live_credential(credential))
}

fn parse_gemini_live_credential(value: &str) -> GeminiLiveCredential {
    if let Some(token) = value.strip_prefix("oauth:") {
        return GeminiLiveCredential::OAuthBearer(token.to_string());
    }
    if let Some(token) = value.strip_prefix("bearer:") {
        return GeminiLiveCredential::OAuthBearer(token.to_string());
    }
    if let Some(token) = value.strip_prefix("ephemeral:") {
        return GeminiLiveCredential::EphemeralToken(token.to_string());
    }
    GeminiLiveCredential::ApiKey(value.to_string())
}

fn gemini_live_inputs(request: &LlmRequest) -> Vec<GeminiLiveInput> {
    request
        .modality_inputs
        .iter()
        .filter(gemini_live_input_supported)
        .map(|input| GeminiLiveInput {
            kind: input.kind,
            media_type: input.media_type.clone(),
            bytes: input.bytes.clone(),
        })
        .collect()
}

fn gemini_live_input_supported(input: &&LlmModalityInput) -> bool {
    matches!(
        input.kind,
        LlmModalityInputKind::LiveAudioChunk | LlmModalityInputKind::ScreenFrame
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LlmProviderKind, SpawnConfig};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeGeminiLiveTransport {
        turns: Mutex<Vec<GeminiLiveTurn>>,
    }

    impl GeminiLiveTransport for FakeGeminiLiveTransport {
        fn stream_turn(
            &self,
            turn: GeminiLiveTurn,
            _control: StreamControl,
            on_event: &mut LlmStreamEventSink<'_>,
        ) -> Result<()> {
            self.turns.lock().unwrap().push(turn);
            on_event(LlmStreamEvent::ContentDelta {
                text: "live answer".to_string(),
            })?;
            on_event(LlmStreamEvent::Complete)
        }
    }

    #[test]
    fn routes_live_audio_and_screen_frame_to_gemini_live_transport() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("provider config should be valid");
        let events = provider
            .stream(&live_request(), StreamControl::unbounded())
            .expect("live stream should use fake transport");

        assert_eq!(
            events,
            vec![
                LlmStreamEvent::ContentDelta {
                    text: "live answer".to_string()
                },
                LlmStreamEvent::Complete
            ]
        );
        let turns = fake.turns.lock().unwrap();
        assert_eq!(
            turns[0].credential,
            GeminiLiveCredential::ApiKey("key".to_string())
        );
        assert_eq!(turns[0].inputs.len(), 2);
    }

    #[test]
    fn live_transport_requires_gemini_api_key() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(None), fake)
            .expect("spawn config should still be valid");

        let error = provider
            .stream(&live_request(), StreamControl::unbounded())
            .expect_err("live request without API key should fail");

        assert!(error.to_string().contains("Gemini API key"));
    }

    #[test]
    fn live_transport_accepts_prefixed_oauth_credential() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(
            config(Some("oauth:access-token")),
            fake.clone(),
        )
        .expect("spawn config should still be valid");

        provider
            .stream(&live_request(), StreamControl::unbounded())
            .expect("OAuth credential should satisfy Live auth preflight");

        let turns = fake.turns.lock().unwrap();
        assert_eq!(
            turns[0].credential,
            GeminiLiveCredential::OAuthBearer("access-token".to_string())
        );
    }

    #[test]
    fn live_transport_receives_assistant_mcp_servers() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("spawn config should still be valid");
        let mut request = live_request();
        request.mcp_servers = vec![crate::llm_providers::LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a".to_string(),
        }];

        provider
            .stream(&request, StreamControl::unbounded())
            .expect("live transport should receive assistant MCP servers");

        let turns = fake.turns.lock().unwrap();
        assert_eq!(turns[0].mcp_servers.len(), 1);
        assert_eq!(turns[0].mcp_servers[0].name, "lumvise-assistant");
        assert_eq!(
            turns[0].mcp_servers[0].url,
            "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a"
        );
    }

    #[test]
    fn live_transport_receives_existing_provider_session_id() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("spawn config should still be valid");
        let mut request = live_request();
        request.provider_session_id = Some("gemini-live-session-1".to_string());

        provider
            .stream(&request, StreamControl::unbounded())
            .expect("live transport should receive resumption handle");

        let turns = fake.turns.lock().unwrap();
        assert_eq!(
            turns[0].session_resumption_handle,
            Some("gemini-live-session-1".to_string())
        );
    }

    #[test]
    fn explicit_non_live_model_fails_before_live_transport() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("spawn config should still be valid");
        let mut request = live_request();
        request.model = Some("gemini-2.5-flash".to_string());

        let error = provider
            .stream(&request, StreamControl::unbounded())
            .expect_err("explicit non-live model should fail locally");

        assert!(error.to_string().contains("Gemini Live API model"));
        assert!(fake.turns.lock().unwrap().is_empty());
    }

    #[test]
    fn empty_live_audio_fails_before_live_transport() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("spawn config should still be valid");
        let mut request = live_request();
        request.modality_inputs[0].bytes.clear();

        let error = provider
            .stream(&request, StreamControl::unbounded())
            .expect_err("empty live audio must fail validation");

        assert!(error.to_string().contains("non-empty LLM modality bytes"));
        assert!(fake.turns.lock().unwrap().is_empty());
    }

    #[test]
    fn image_snapshot_fails_before_live_transport() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("spawn config should still be valid");
        let mut request = live_request();
        request.modality_inputs = vec![image_snapshot_input()];

        let error = provider
            .stream(&request, StreamControl::unbounded())
            .expect_err("Gemini should not silently ignore image snapshots");

        assert!(
            error
                .to_string()
                .contains("image_snapshot_input is unsupported")
        );
        assert!(fake.turns.lock().unwrap().is_empty());
    }

    #[test]
    fn provider_default_non_live_model_uses_live_default_for_live_request() {
        let fake = Arc::new(FakeGeminiLiveTransport::default());
        let provider = GeminiProvider::new_with_live_transport(config(Some("key")), fake.clone())
            .expect("spawn config should still be valid");
        let mut request = live_request();
        request.model = None;

        provider
            .stream(&request, StreamControl::unbounded())
            .expect("implicit model should use Gemini Live default");

        let turns = fake.turns.lock().unwrap();
        assert_eq!(turns[0].model, DEFAULT_GEMINI_LIVE_MODEL);
    }

    fn live_request() -> LlmRequest {
        LlmRequest {
            options: Default::default(),
            messages: vec![crate::llm_providers::LlmMessage {
                role: "user".to_string(),
                content: "what is on screen?".to_string(),
            }],
            stream: true,
            provider_id: None,
            model: Some("gemini-3.1-flash-live-preview".to_string()),
            conversation_id: None,
            provider_session_id: None,
            mcp_servers: vec![],
            modality_inputs: vec![audio_input(), screen_frame_input()],
        }
    }

    fn audio_input() -> LlmModalityInput {
        LlmModalityInput {
            input_id: "audio-1".to_string(),
            kind: LlmModalityInputKind::LiveAudioChunk,
            media_type: "audio/pcm;rate=16000".to_string(),
            bytes: vec![1, 2],
            metadata: json!({}),
        }
    }

    fn screen_frame_input() -> LlmModalityInput {
        LlmModalityInput {
            input_id: "screen-1".to_string(),
            kind: LlmModalityInputKind::ScreenFrame,
            media_type: "image/jpeg".to_string(),
            bytes: vec![3, 4],
            metadata: json!({}),
        }
    }

    fn image_snapshot_input() -> LlmModalityInput {
        LlmModalityInput {
            input_id: "image-1".to_string(),
            kind: LlmModalityInputKind::ImageSnapshot,
            media_type: "image/jpeg".to_string(),
            bytes: vec![3, 4],
            metadata: json!({}),
        }
    }

    fn config(credential: Option<&str>) -> LlmProviderConfig {
        LlmProviderConfig {
            provider_id: "gemini".to_string(),
            kind: LlmProviderKind::Gemini,
            model: "gemini-2.5-flash".to_string(),
            endpoint: None,
            credential: credential.map(str::to_string),
            completion_concurrency: None,
            spawn: Some(SpawnConfig {
                command: "printf".to_string(),
                args: vec!["{}".to_string()],
                timeout_ms: 1000,
            }),
        }
    }
}
