use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::adapter::LlmTransportKind;
use crate::llm_providers::capabilities::claude_provider_capabilities;
use crate::llm_providers::claude::api::ClaudeDirectApiTransport;
use crate::llm_providers::cli_output::json_field_text;
use crate::llm_providers::cli_stream::CliJsonTextStreamState;
use crate::llm_providers::command_runner::{
    ProviderCommandRunner, cli_spawn, model_args as command_model_args,
    prompt_text as command_prompt_text, replace_arg_value, response, selected_model,
};
use crate::llm_providers::contract::{
    LlmHttpClient, LlmProvider, LlmStreamEventSink, ProviderCallControl,
};
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::tool_invocation::McpToolCatalog;
use crate::llm_providers::{
    LlmMcpServerConfig, LlmModalityInput, LlmModalityInputKind, LlmProviderCapabilities,
    LlmRequest, LlmResponse, LlmStreamEvent,
};
use crate::process::StreamControl;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub mod api;

pub struct ClaudeProvider {
    config: LlmProviderConfig,
    runner: Option<ProviderCommandRunner>,
    direct_api: Option<ClaudeDirectApiTransport>,
    selected_transport: LlmTransportKind,
}

impl ClaudeProvider {
    pub fn new(config: LlmProviderConfig) -> Result<Self> {
        Self::new_with_transport(
            config,
            Arc::new(NoClaudeHttpClient),
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
        let direct_api = match selected_transport {
            LlmTransportKind::DirectApi => {
                Some(ClaudeDirectApiTransport::new(config.clone(), http_client))
            }
            _ => None,
        };
        Ok(Self {
            config,
            runner,
            direct_api,
            selected_transport,
        })
    }
}

impl LlmProvider for ClaudeProvider {
    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        claude_provider_capabilities(&self.config.provider_id)
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        if let Some(direct_api) = &self.direct_api {
            return direct_api.complete(request);
        }
        validate_llm_request(request)?;
        let snapshots = ClaudeImageSnapshotFiles::new(&self.config, request)?;
        let session_id = claude_session_id(request);
        let output = self.client_runner()?.run(
            claude_args(&self.config, request, &snapshots, &session_id),
            None,
        )?;
        let content = json_field_text("Claude", &output.stdout, "result")?;
        Ok(response(
            &self.config,
            request,
            content,
            json!({ "provider_session_id": session_id, "transport": "client" }),
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
        let snapshots = ClaudeImageSnapshotFiles::new(&self.config, request)?;
        let session_id = claude_session_id(request);
        let output = self.client_runner()?.run_cancellable(
            claude_args(&self.config, request, &snapshots, &session_id),
            None,
            control,
        )?;
        let content = json_field_text("Claude", &output.stdout, "result")?;
        Ok(response(
            &self.config,
            request,
            content,
            json!({ "provider_session_id": session_id, "transport": "client" }),
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
        let snapshots = ClaudeImageSnapshotFiles::new(&self.config, request)?;
        let mut state = CliJsonTextStreamState::new(control);
        let session_id = claude_session_id(request);
        on_event(LlmStreamEvent::Session {
            provider_session_id: session_id.clone(),
        })?;
        self.client_runner()?.run_stdout_lines(
            claude_stream_args(&self.config, request, &snapshots, &session_id),
            None,
            &mut |line| state.push_line("Claude", &line, on_event),
        )?;
        if state.cancelled() {
            return on_event(LlmStreamEvent::Cancelled);
        }
        on_event(LlmStreamEvent::Complete)
    }
}

impl ClaudeProvider {
    fn client_runner(&self) -> Result<&ProviderCommandRunner> {
        self.runner
            .as_ref()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: format!("{:?}", self.selected_transport),
                expected: "Claude client transport".to_string(),
            })
    }
}

struct NoClaudeHttpClient;

impl LlmHttpClient for NoClaudeHttpClient {
    fn post_json(
        &self,
        _request: &crate::llm_providers::contract::LlmHttpRequest,
    ) -> Result<serde_json::Value> {
        unreachable!("Claude client mode does not use HTTP")
    }

    fn stream_text(
        &self,
        _request: &crate::llm_providers::contract::LlmHttpRequest,
    ) -> Result<Vec<String>> {
        unreachable!("Claude client mode does not use HTTP")
    }
}

fn claude_args(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    snapshots: &ClaudeImageSnapshotFiles,
    session_id: &str,
) -> Vec<String> {
    let mut args = vec!["-p".to_string(), provider_prompt(request, snapshots)];
    args.extend(model_args(&selected_model(config, request)));
    args.extend(claude_session_args(request, session_id));
    args.extend(["--output-format".to_string(), "json".to_string()]);
    args.extend(claude_permission_args(request));
    args.extend(claude_mcp_args(config, request));
    args
}

fn claude_session_args(request: &LlmRequest, session_id: &str) -> Vec<String> {
    if request.provider_session_id.is_some() {
        return vec!["--resume".to_string(), session_id.to_string()];
    }
    vec!["--session-id".to_string(), session_id.to_string()]
}

fn claude_permission_args(request: &LlmRequest) -> Vec<String> {
    let mode = if request.mcp_servers.is_empty() {
        "plan"
    } else {
        "default"
    };
    vec!["--permission-mode".to_string(), mode.to_string()]
}

fn claude_mcp_args(config: &LlmProviderConfig, request: &LlmRequest) -> Vec<String> {
    let mut args = isolated_mcp_args(&request.mcp_servers);
    if !request.mcp_servers.is_empty() {
        args.extend([
            "--allowedTools".to_string(),
            claude_allowed_mcp_tools(config, request).join(","),
        ]);
    }
    args
}

/// Keeps app-owned turns and probes from starting personal MCP processes.
/// An availability probe uses `isolated_mcp_args(&[])`.
pub(super) fn isolated_mcp_args(servers: &[LlmMcpServerConfig]) -> Vec<String> {
    vec![
        "--mcp-config".to_string(),
        claude_mcp_config_json(servers),
        "--strict-mcp-config".to_string(),
    ]
}

/// Lumvise's scoped/plugin MCP endpoints serve request/response JSON-RPC over
/// a single `POST` route (the MCP "Streamable HTTP" transport), not the older
/// two-endpoint SSE transport. Declaring `"type": "sse"` here made Claude
/// Code's client GET the message URL expecting an SSE handshake, which 404s
/// and drops the server (and every one of its tools) before any turn starts;
/// `"type": "http"` matches what the server actually implements.
fn claude_mcp_config_json(servers: &[LlmMcpServerConfig]) -> String {
    serde_json::to_string(&serde_json::json!({
        "mcpServers": servers.iter().map(|server| {
            (
                server.name.clone(),
                serde_json::json!({ "type": "http", "url": server.url }),
            )
        }).collect::<serde_json::Map<_, _>>()
    }))
    .unwrap_or_else(|_| "{\"mcpServers\":{}}".to_string())
}

/// Claude Code's `--allowedTools` wildcard (`mcp__server__*`) silently fails to
/// match MCP tools and every tool call is denied
/// (see anthropics/claude-code#13077, #6010, #2928, #3107 — an open upstream
/// CLI bug as of Claude Code 2.0.53). The documented workaround is to allow-list
/// every real tool name explicitly, so discover each server's tools up front and
/// build `mcp__<server>__<tool>` per tool. Falls back to the (best-effort) wildcard
/// only if discovery itself fails, so a broken lookup never blocks the turn outright.
fn claude_allowed_mcp_tools(config: &LlmProviderConfig, request: &LlmRequest) -> Vec<String> {
    request
        .mcp_servers
        .iter()
        .flat_map(|server| server_allowed_tools(config, server))
        .collect()
}

fn server_allowed_tools(config: &LlmProviderConfig, server: &LlmMcpServerConfig) -> Vec<String> {
    let discovered = McpToolCatalog::discover(&config.provider_id, std::slice::from_ref(server))
        .map(|catalog| {
            catalog
                .tools()
                .iter()
                .map(|tool| format!("mcp__{}__{}", server.name, tool.name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if discovered.is_empty() {
        vec![format!("mcp__{}__*", server.name)]
    } else {
        discovered
    }
}

pub(crate) fn model_args(model: &str) -> Vec<String> {
    command_model_args(model)
}

pub(crate) fn provider_prompt(
    request: &LlmRequest,
    snapshots: &ClaudeImageSnapshotFiles,
) -> String {
    let mut prompt = command_prompt_text(request);
    if let Some(snapshot_prompt) = snapshots.prompt() {
        prompt.push_str("\n\n");
        prompt.push_str(snapshot_prompt);
    }
    prompt
}

fn claude_stream_args(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    snapshots: &ClaudeImageSnapshotFiles,
    session_id: &str,
) -> Vec<String> {
    let mut args = claude_args(config, request, snapshots, session_id);
    replace_arg_value(&mut args, "--output-format", "stream-json");
    args.push("--verbose".to_string());
    args.push("--include-partial-messages".to_string());
    args
}

fn claude_session_id(request: &LlmRequest) -> String {
    request
        .provider_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(new_claude_session_id)
}

fn new_claude_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let value = nanos ^ (pid << 48);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (value >> 96) as u32,
        (value >> 80) as u16,
        ((value >> 64) as u16) & 0x0fff,
        ((value >> 48) as u16) & 0x0fff,
        value & 0x0000_ffff_ffff_ffff
    )
}

pub(crate) struct ClaudeImageSnapshotFiles {
    temp_dir: Option<PathBuf>,
    prompt: Option<String>,
}

impl ClaudeImageSnapshotFiles {
    fn new(config: &LlmProviderConfig, request: &LlmRequest) -> Result<Self> {
        validate_claude_modality_inputs(config, request)?;
        let snapshots = image_snapshot_inputs(request);
        if snapshots.is_empty() {
            return Ok(Self {
                temp_dir: None,
                prompt: None,
            });
        }
        let temp_dir = create_snapshot_temp_dir(config)?;
        let prompt = write_snapshot_files(&temp_dir, snapshots)?;
        Ok(Self {
            temp_dir: Some(temp_dir),
            prompt: Some(prompt),
        })
    }

    fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }
}

impl Drop for ClaudeImageSnapshotFiles {
    fn drop(&mut self) {
        let Some(temp_dir) = self.temp_dir.take() else {
            return;
        };
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}

fn validate_claude_modality_inputs(config: &LlmProviderConfig, request: &LlmRequest) -> Result<()> {
    for input in &request.modality_inputs {
        match input.kind {
            LlmModalityInputKind::ImageSnapshot => continue,
            LlmModalityInputKind::LiveAudioChunk => {
                return Err(unsupported_claude_modality(
                    config,
                    &input.input_id,
                    "live_audio_input is unsupported",
                ));
            }
            LlmModalityInputKind::ScreenFrame => {
                return Err(unsupported_claude_modality(
                    config,
                    &input.input_id,
                    "screen_frame_broadcast_input is unsupported",
                ));
            }
        }
    }
    if request.modality_inputs.iter().any(claude_input_is_image)
        && !claude_model_supports_image_snapshot(&selected_model(config, request))
    {
        return Err(NeuralError::InvalidValue {
            value: selected_model(config, request),
            expected: "Claude image-capable model for image_snapshot_input".to_string(),
        });
    }
    Ok(())
}

fn unsupported_claude_modality(
    config: &LlmProviderConfig,
    input_id: &str,
    expected: &str,
) -> NeuralError {
    NeuralError::InvalidValue {
        value: format!("{}:{input_id}", config.provider_id),
        expected: format!("Claude image_snapshot_input or text-only request; {expected}"),
    }
}

fn image_snapshot_inputs(request: &LlmRequest) -> Vec<&LlmModalityInput> {
    request
        .modality_inputs
        .iter()
        .filter(|input| claude_input_is_image(input))
        .collect()
}

fn claude_input_is_image(input: &LlmModalityInput) -> bool {
    input.kind == LlmModalityInputKind::ImageSnapshot
}

fn claude_model_supports_image_snapshot(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    ["claude-3", "claude-4", "sonnet", "opus", "haiku", "fable"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn create_snapshot_temp_dir(config: &LlmProviderConfig) -> Result<PathBuf> {
    let temp_dir = std::env::temp_dir().join(snapshot_temp_dir_name(config));
    std::fs::create_dir(&temp_dir).map_err(|source| NeuralError::Io {
        value: temp_dir.display().to_string(),
        expected: "Claude image snapshot temp directory".to_string(),
        source,
    })?;
    Ok(temp_dir)
}

fn snapshot_temp_dir_name(config: &LlmProviderConfig) -> String {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "lumvise-{}-claude-snapshots-{}-{suffix}",
        config.provider_id,
        std::process::id()
    )
}

fn write_snapshot_files(
    temp_dir: &std::path::Path,
    snapshots: Vec<&LlmModalityInput>,
) -> Result<String> {
    let mut prompt = String::from("Image snapshot inputs:\n");
    for (index, input) in snapshots.iter().enumerate() {
        let path = temp_dir.join(snapshot_file_name(index, input));
        std::fs::write(&path, &input.bytes).map_err(|source| NeuralError::Io {
            value: path.display().to_string(),
            expected: "writable Claude image snapshot file".to_string(),
            source,
        })?;
        prompt.push_str(&snapshot_prompt_entry(input, &path));
    }
    prompt.push_str("Use the Read tool on these paths when visual context is relevant.\n");
    Ok(prompt)
}

fn snapshot_file_name(index: usize, input: &LlmModalityInput) -> String {
    format!(
        "{index}-{}.{}",
        sanitized_snapshot_id(&input.input_id),
        snapshot_extension(&input.media_type)
    )
}

fn sanitized_snapshot_id(input_id: &str) -> String {
    input_id
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => character,
            _ => '-',
        })
        .collect()
}

fn snapshot_extension(media_type: &str) -> &'static str {
    let normalized = media_type.to_ascii_lowercase();
    if normalized.contains("png") {
        return "png";
    }
    if normalized.contains("jpeg") || normalized.contains("jpg") {
        return "jpg";
    }
    if normalized.contains("webp") {
        return "webp";
    }
    "img"
}

fn snapshot_prompt_entry(input: &LlmModalityInput, path: &std::path::Path) -> String {
    format!(
        "- id: {}\n  media_type: {}\n  path: {}\n",
        input.input_id,
        input.media_type,
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmProviderKind;
    use serde_json::Value;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};

    struct FakeMcpServer {
        url: String,
        join: Option<JoinHandle<()>>,
    }

    impl FakeMcpServer {
        fn spawn(responses: Vec<Value>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "http://{}/mcp/sse/assistant",
                listener.local_addr().unwrap()
            );
            let join = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    read_request(&mut stream);
                    let body = response.to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            });
            Self {
                url,
                join: Some(join),
            }
        }
    }

    impl Drop for FakeMcpServer {
        fn drop(&mut self) {
            self.join.take().unwrap().join().unwrap();
        }
    }

    fn read_request(stream: &mut TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
    }

    fn claude_config() -> LlmProviderConfig {
        LlmProviderConfig {
            provider_id: "claude".into(),
            kind: LlmProviderKind::Claude,
            model: "sonnet".into(),
            endpoint: None,
            credential: None,
            completion_concurrency: None,
            spawn: None,
        }
    }

    fn tool_list(names: &[&str]) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "tools": names.iter().map(|name| serde_json::json!({
                "name": name,
                "description": "d",
                "inputSchema": { "type": "object" }
            })).collect::<Vec<_>>() }
        })
    }

    #[test]
    fn allowed_mcp_tools_enumerates_real_tool_names_instead_of_the_broken_wildcard() {
        let server = FakeMcpServer::spawn(vec![tool_list(&["canvas_get", "knowledge_search"])]);
        let server_config = LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: server.url.clone(),
        };

        let allowed = server_allowed_tools(&claude_config(), &server_config);

        // Claude Code's `--allowedTools mcp__server__*` wildcard silently fails to
        // match MCP tools (anthropics/claude-code#13077); every entry here must be
        // an exact `mcp__<server>__<tool>` name discovered from the live server.
        assert_eq!(
            allowed,
            vec![
                "mcp__lumvise-assistant__canvas_get".to_string(),
                "mcp__lumvise-assistant__knowledge_search".to_string(),
            ]
        );
        assert!(!allowed.iter().any(|tool| tool.ends_with("__*")));
    }

    #[test]
    fn allowed_mcp_tools_falls_back_to_wildcard_when_discovery_fails() {
        let unreachable_server = LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: "http://127.0.0.1:1".to_string(),
        };

        let allowed = server_allowed_tools(&claude_config(), &unreachable_server);

        assert_eq!(allowed, vec!["mcp__lumvise-assistant__*".to_string()]);
    }
}
