use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::adapter::LlmTransportKind;
use crate::llm_providers::capabilities::codex_provider_capabilities;
use crate::llm_providers::cli_output::parse_text_payload;
use crate::llm_providers::cli_stream::stream_json_text_delta;
use crate::llm_providers::codex::api::OpenAiDirectApiTransport;
use crate::llm_providers::command_runner::{ProviderCommandRunner, model_args, prompt_text};
use crate::llm_providers::contract::{
    LlmHttpClient, LlmProvider, LlmStreamEventSink, ProviderCallControl,
};
use crate::llm_providers::local::{validate_llm_request, validate_text_only_modality_inputs};
use crate::llm_providers::tool_invocation::mcp_message_url;
use crate::llm_providers::{LlmProviderCapabilities, LlmRequest, LlmResponse, LlmStreamEvent};
use crate::process::StreamControl;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod api;
mod app_server;
pub mod live;
mod persistent;
use app_server::CodexAppServerSession;
mod scoped_tool_policy;
use scoped_tool_policy::codex_scoped_tool_approval;

static CODEX_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const CODEX_NO_SESSION: &str = "-";
const CODEX_ERROR_PREFIX: &str = "lumvise-codex-error\t";
const CODEX_SESSION_PREFIX: &str = "lumvise-codex-session-id\t";
const CODEX_MARKER_PREFIX: &str = "lumvise-codex-stream-marker-";
const CODEX_PROVIDER_DEFAULT_MODEL: &str = "provider-default";
// App-owned turns already communicate through scoped MCP. Inheriting the
// desktop agent's completion hook recursively opens sessions, even for probes.
pub(super) const APP_OWNED_CODEX_NOTIFY: &str = "notify=[]";

pub struct CodexProvider {
    config: LlmProviderConfig,
    runner: Option<ProviderCommandRunner>,
    direct_api: Option<OpenAiDirectApiTransport>,
    selected_transport: LlmTransportKind,
    hot_session: Mutex<Option<CodexHotSession>>,
    app_server_sessions: Mutex<persistent::CodexSessions>,
}

impl CodexProvider {
    /// Creates a spawn-backed Codex CLI provider.
    ///
    /// # Example
    ///
    /// ```
    /// # use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, SpawnConfig};
    /// # use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
    /// # use serde_json::Value;
    /// # use std::sync::Arc;
    /// # struct NoHttp;
    /// # impl LlmHttpClient for NoHttp {
    /// #   fn post_json(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> { unreachable!() }
    /// #   fn stream_text(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> { unreachable!() }
    /// # }
    /// let config = LlmProviderConfig {
    ///     provider_id: "codex".into(), kind: LlmProviderKind::Codex, model: "gpt-5".into(),
    ///     endpoint: None, credential: None, completion_concurrency: None,
    ///     spawn: Some(SpawnConfig { command: "codex".into(), args: vec![], timeout_ms: 1000 }),
    /// };
    /// assert!(lumvise_neural_core::llm_providers::codex::CodexProvider::new(
    ///     config, Arc::new(NoHttp)
    /// ).is_ok());
    /// ```
    pub fn new(config: LlmProviderConfig, http_client: Arc<dyn LlmHttpClient>) -> Result<Self> {
        let transport = if config.spawn.is_some() {
            LlmTransportKind::Client
        } else {
            LlmTransportKind::DirectApi
        };
        Self::new_with_transport(config, http_client, transport)
    }

    pub fn new_with_transport(
        config: LlmProviderConfig,
        http_client: Arc<dyn LlmHttpClient>,
        selected_transport: LlmTransportKind,
    ) -> Result<Self> {
        config.validate()?;
        let runner = match selected_transport {
            LlmTransportKind::Client => Some(ProviderCommandRunner::new(
                config.spawn.clone().ok_or_else(|| missing_spawn("codex"))?,
            )?),
            _ => None,
        };
        let direct_api = match selected_transport {
            LlmTransportKind::DirectApi => {
                Some(OpenAiDirectApiTransport::new(config.clone(), http_client))
            }
            _ => None,
        };
        Ok(Self {
            config,
            runner,
            direct_api,
            selected_transport,
            hot_session: Mutex::new(None),
            app_server_sessions: Mutex::new(persistent::CodexSessions::new()),
        })
    }
}

impl LlmProvider for CodexProvider {
    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        codex_provider_capabilities(&self.config.provider_id)
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        if let Some(direct_api) = &self.direct_api {
            return direct_api.complete(request);
        }
        validate_llm_request(request)?;
        validate_text_only_modality_inputs(&self.config.provider_id, request)?;
        let output = self.run_turn(request, None)?;
        Ok(response(&self.config, request, output, self.hot_enabled()))
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
            return direct_api.complete(request);
        }
        validate_llm_request(request)?;
        validate_text_only_modality_inputs(&self.config.provider_id, request)?;
        let output = self.run_turn(request, Some(control))?;
        Ok(response(&self.config, request, output, self.hot_enabled()))
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
        validate_text_only_modality_inputs(&self.config.provider_id, request)?;
        if self.hot_enabled() && !request.mcp_servers.is_empty() {
            return self
                .app_server_turn(request, None, control, on_event)
                .map(|_| ());
        }
        self.stateless_stream(request, control, on_event)
    }
}

impl CodexProvider {
    fn run_turn(
        &self,
        request: &LlmRequest,
        control: Option<&dyn ProviderCallControl>,
    ) -> Result<CodexTurnOutput> {
        if self.hot_enabled() && !request.mcp_servers.is_empty() {
            return self.app_server_turn(request, control, StreamControl::unbounded(), &mut |_| {
                Ok(())
            });
        }
        if self.hot_enabled() && request.model_id().is_none() && request.mcp_servers.is_empty() {
            return self.hot_turn(request);
        }
        self.stateless_turn(request, control)
    }

    fn hot_turn(&self, request: &LlmRequest) -> Result<CodexTurnOutput> {
        let mut guard = self
            .hot_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(CodexHotSession::spawn(
                self.client_runner()?.command(),
                &self.config,
            )?);
        }
        if let Some(session) = guard.as_mut() {
            session.set_provider_session_id(request.provider_session_id.clone());
        }
        let result = guard
            .as_mut()
            .expect("codex hot session exists")
            .run_turn(&prompt_text(request), &self.config);
        if result.is_err() {
            *guard = None;
        }
        result
    }

    fn stateless_turn(
        &self,
        request: &LlmRequest,
        control: Option<&dyn ProviderCallControl>,
    ) -> Result<CodexTurnOutput> {
        let output_path = unique_output_path("codex-output");
        let output = match control {
            Some(control) => self.client_runner()?.run_cancellable(
                codex_stateless_args(&self.config, request, &output_path),
                None,
                control,
            ),
            None => self.client_runner()?.run(
                codex_stateless_args(&self.config, request, &output_path),
                None,
            ),
        };
        let file_output = std::fs::read_to_string(&output_path).unwrap_or_default();
        let _ = std::fs::remove_file(&output_path);
        let output = output?;
        Ok(CodexTurnOutput {
            // A successful MCP turn can deliver its answer through tools only.
            // The caller owns validation of those effects, not the CLI adapter.
            content: if file_output.trim().is_empty() && !request.mcp_servers.is_empty() {
                String::new()
            } else {
                parse_codex_output(&file_output)?
            },
            provider_session_id: codex_session_id_from_diagnostics(&output.stdout)
                .or_else(|| codex_session_id_from_diagnostics(&output.stderr))
                .or_else(|| request.provider_session_id.clone()),
        })
    }

    fn stateless_stream(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        let output_path = unique_output_path("codex-output");
        let stderr_path = unique_output_path("codex-stderr");
        let mut process =
            CodexStreamProcess::spawn(&self.config, request, &output_path, &stderr_path)?;
        let result = process.drain(control.clone(), on_event);
        let file_output = std::fs::read_to_string(&output_path).unwrap_or_default();
        let _ = std::fs::remove_file(&output_path);
        let _ = std::fs::remove_file(&stderr_path);
        result.and_then(|state| emit_codex_fallback(file_output, state, control, on_event))
    }

    fn hot_enabled(&self) -> bool {
        let Ok(runner) = self.client_runner() else {
            return false;
        };
        Path::new(runner.command())
            .file_name()
            .is_some_and(|name| name == "codex")
    }

    fn client_runner(&self) -> Result<&ProviderCommandRunner> {
        self.runner
            .as_ref()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: format!("{:?}", self.selected_transport),
                expected: "Codex client transport".to_string(),
            })
    }
}

mod stream_session;
use stream_session::{CodexHotSession, CodexStreamProcess, CodexTurnOutput};

fn codex_stateless_args(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    output_path: &Path,
) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--skip-git-repo-check".to_string(),
        "--sandbox".to_string(),
        "read-only".to_string(),
        "--json".to_string(),
        "-o".to_string(),
        output_path.to_string_lossy().into_owned(),
    ];
    args.extend(codex_client_config_args(config, request));
    args.extend(codex_mcp_server_args(request));
    if let Some(session_id) = request.provider_session_id.as_deref() {
        args.push("resume".to_string());
        args.push(session_id.to_string());
    }
    args.push(prompt_text(request));
    args
}

fn codex_app_server_process_args(config: &LlmProviderConfig, request: &LlmRequest) -> Vec<String> {
    let mut args = Vec::new();
    args.extend(codex_client_config_args(config, request));
    args.push("app-server".to_string());
    args
}

fn isolated_codex_home(request: &LlmRequest) -> Result<Option<PathBuf>> {
    let Some(source_home) = resolve_existing_codex_home() else {
        return Ok(None);
    };
    let target_home = unique_codex_home_path();
    std::fs::create_dir_all(&target_home).map_err(|source| NeuralError::Io {
        value: target_home.display().to_string(),
        expected: "temporary Codex home directory".to_string(),
        source,
    })?;
    copy_codex_auth_file(&source_home, &target_home)?;
    write_isolated_codex_config(&source_home, &target_home, request)?;
    Ok(Some(target_home))
}

fn resolve_existing_codex_home() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME")
        && !codex_home.is_empty()
    {
        return Some(PathBuf::from(codex_home));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".codex"))
}

fn unique_codex_home_path() -> PathBuf {
    let sequence = CODEX_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("lumvise-codex-home-{millis}-{sequence}"))
}

fn copy_codex_auth_file(source_home: &Path, target_home: &Path) -> Result<()> {
    let source = source_home.join("auth.json");
    if !source.exists() {
        return Ok(());
    }
    std::fs::copy(&source, target_home.join("auth.json")).map_err(|error| NeuralError::Io {
        value: source.display().to_string(),
        expected: "copyable Codex auth.json".to_string(),
        source: error,
    })?;
    Ok(())
}

fn write_isolated_codex_config(
    source_home: &Path,
    target_home: &Path,
    request: &LlmRequest,
) -> Result<()> {
    let source = source_home.join("config.toml");
    let config = std::fs::read_to_string(&source).unwrap_or_default();
    let mut isolated = sanitize_codex_config_without_mcp_servers(&config);
    isolated.push_str(&codex_mcp_config_toml(request));
    std::fs::write(target_home.join("config.toml"), isolated).map_err(|error| NeuralError::Io {
        value: target_home.display().to_string(),
        expected: "writable isolated Codex config.toml".to_string(),
        source: error,
    })
}

fn codex_mcp_config_toml(request: &LlmRequest) -> String {
    if request.mcp_servers.is_empty() {
        return String::new();
    }
    let mut config = String::new();
    for server in &request.mcp_servers {
        let server_key = toml_key_segment(&server.name);
        config.push_str("\n[mcp_servers.");
        config.push_str(&server_key);
        config.push_str("]\n");
        config.push_str("enabled = true\n");
        // The trusted Assistant scope has no interactive approver. `auto`
        // can still prompt; `approve` expresses the app's standing grant.
        config.push_str(&format!(
            "default_tools_approval_mode = {}\n",
            toml_string(codex_scoped_tool_approval(server))
        ));
        // Canvas and voice host-calls round-trip through the renderer
        // (whiteboard sync, TTS playback handoff), which can take several
        // seconds per call under load. Codex's default MCP call window is
        // shorter than that and cancels the call mid-flight, which the model
        // reports as a failed canvas update (#96).
        config.push_str("tool_timeout_sec = 120\n");
        // `url` selects Codex's streamable-HTTP transport, whose request
        // endpoint is the scoped MCP message route rather than its SSE
        // discovery endpoint.
        config.push_str("url = ");
        config.push_str(&toml_string(&mcp_message_url(&server.url)));
        config.push('\n');
    }
    config
}

fn sanitize_codex_config_without_mcp_servers(config: &str) -> String {
    let mut result = Vec::new();
    let mut skipping_mcp_section = false;
    let mut in_section = false;
    for line in config.lines() {
        let trimmed = strip_toml_comment(line).trim().to_string();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            skipping_mcp_section = toml_header_is_mcp_server(&trimmed);
        }
        if !skipping_mcp_section && !top_level_codex_session_setting(&trimmed, in_section) {
            result.push(line);
        }
    }
    result.join("\n")
}

fn toml_header_is_mcp_server(header: &str) -> bool {
    let section = header.trim_start_matches('[').trim_end_matches(']').trim();
    section == "mcp_servers" || section.starts_with("mcp_servers.")
}

fn top_level_codex_session_setting(line: &str, in_section: bool) -> bool {
    if in_section || line.is_empty() {
        return false;
    }
    let Some((key, _)) = line.split_once('=') else {
        return false;
    };
    matches!(
        key.trim(),
        "model"
            | "model_provider"
            | "model_reasoning_effort"
            | "model_reasoning_summary"
            | "model_verbosity"
    )
}

fn hot_shell_script(command: &str, config: &LlmProviderConfig) -> String {
    let model = codex_model_argument(&config.model);
    let spawn_args = config
        .spawn
        .as_ref()
        .map(|spawn| spawn.args.as_slice())
        .unwrap_or(&[]);
    format!(
        "{}{}{}",
        hot_shell_header(),
        hot_shell_body(command, spawn_args, model),
        hot_shell_footer()
    )
}

fn hot_shell_header() -> &'static str {
    "set -u\nwhile IFS=$'\\t' read -r marker session_id prompt_path; do\n  prompt=$(cat \"$prompt_path\")\n  rm -f \"$prompt_path\"\n  output_path=$(mktemp)\n  diagnostics_path=$(mktemp)\n"
}

fn hot_shell_body(command: &str, spawn_args: &[String], model: Option<&str>) -> String {
    let mut script = String::new();
    script.push_str("  if [ \"$session_id\" != '");
    script.push_str(CODEX_NO_SESSION);
    script.push_str("' ]; then\n    ");
    append_codex_shell_command(&mut script, command, spawn_args, "exec resume", model);
    script.push_str(" \"$session_id\" \"$prompt\" < /dev/null > \"$diagnostics_path\" 2>&1\n");
    script.push_str("  else\n    ");
    append_codex_shell_command(&mut script, command, spawn_args, "exec", model);
    script.push_str(" \"$prompt\" < /dev/null > \"$diagnostics_path\" 2>&1\n  fi\n");
    script
}

fn append_codex_shell_command(
    script: &mut String,
    command: &str,
    spawn_args: &[String],
    mode: &str,
    model: Option<&str>,
) {
    script.push_str(&shell_quote(command));
    for arg in spawn_args {
        script.push(' ');
        script.push_str(&shell_quote(arg));
    }
    script.push(' ');
    script.push_str(mode);
    script.push_str(" -c ");
    script.push_str(&shell_quote(APP_OWNED_CODEX_NOTIFY));
    script.push_str(" --skip-git-repo-check --json -o \"$output_path\"");
    if let Some(model) = model {
        script.push_str(" --model ");
        script.push_str(&shell_quote(model));
    }
}

fn hot_shell_footer() -> &'static str {
    "  status=$?\n  if [ $status -ne 0 ]; then printf 'lumvise-codex-error\\tstatus %s: ' \"$status\"; tr '\\n' ' ' < \"$diagnostics_path\"; printf '\\n'; rm -f \"$output_path\" \"$diagnostics_path\"; echo \"$marker\"; continue; fi\n  next_session_id=$(sed -n 's/.*\"type\":\"session_meta\".*\"id\":\"\\([^\"]*\\)\".*/\\1/p' \"$diagnostics_path\" | tail -n 1)\n  if [ -n \"$next_session_id\" ]; then printf 'lumvise-codex-session-id\\t%s\\n' \"$next_session_id\"; fi\n  cat \"$output_path\"\n  printf '\\n'\n  rm -f \"$output_path\" \"$diagnostics_path\"\n  echo \"$marker\"\ndone\n"
}

fn emit_stream_events(
    content: String,
    control: StreamControl,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    on_event(LlmStreamEvent::FinalText { text: content })?;
    if control.should_cancel_after(1) {
        return on_event(LlmStreamEvent::Cancelled);
    }
    on_event(LlmStreamEvent::Complete)
}

#[derive(Default)]
struct CodexStreamState {
    content_events: usize,
    terminal_emitted: bool,
    session_emitted: bool,
}

impl CodexStreamState {
    fn emit_content(
        &mut self,
        text: String,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        if self.terminal_emitted || text.trim().is_empty() {
            return Ok(());
        }
        on_event(LlmStreamEvent::ContentDelta { text })?;
        self.content_events += 1;
        if control.should_cancel_after(self.content_events) {
            on_event(LlmStreamEvent::Cancelled)?;
            self.terminal_emitted = true;
        }
        Ok(())
    }

    fn emit_complete(&mut self, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        if self.terminal_emitted {
            return Ok(());
        }
        on_event(LlmStreamEvent::Complete)?;
        self.terminal_emitted = true;
        Ok(())
    }

    fn emitted_content(&self) -> bool {
        self.content_events > 0
    }
}

fn emit_codex_json_line(
    line: &str,
    control: StreamControl,
    state: &mut CodexStreamState,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(());
    };
    if let Some(message) = codex_error_message(&value) {
        return Err(NeuralError::ProviderFailed {
            provider_id: "codex".into(),
            message,
        });
    }
    if let Some(session_id) = codex_thread_id_from_value(&value)
        && !state.session_emitted
    {
        on_event(LlmStreamEvent::Session {
            provider_session_id: session_id,
        })?;
        state.session_emitted = true;
    }
    if codex_json_is_complete(&value) {
        return state.emit_complete(on_event);
    }
    if let Some(text) = codex_json_text(&value, state.emitted_content()) {
        return state.emit_content(text, control, on_event);
    }
    Ok(())
}

fn emit_codex_fallback(
    file_output: String,
    mut state: CodexStreamState,
    control: StreamControl,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    if state.terminal_emitted {
        return Ok(());
    }
    if state.emitted_content() {
        return state.emit_complete(on_event);
    }
    let content = parse_codex_output(&file_output)?;
    emit_stream_events(content, control, on_event)
}

fn codex_json_is_complete(value: &serde_json::Value) -> bool {
    matches!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("turn.completed")
    )
}

fn codex_json_text(value: &serde_json::Value, emitted_content: bool) -> Option<String> {
    let kind = value.get("type").and_then(serde_json::Value::as_str)?;
    match kind {
        "item.completed" | "item.updated" if emitted_content => None,
        "item.completed" => codex_completed_item_text(value),
        "item.updated" => {
            codex_stream_delta_text(value).or_else(|| parse_text_payload(&value.to_string()))
        }
        "item.delta" | "agent_message.delta" => codex_stream_delta_text(value),
        _ => None,
    }
}

fn codex_stream_delta_text(value: &serde_json::Value) -> Option<String> {
    stream_json_text_delta("codex", &value.to_string())
        .ok()
        .flatten()
}

fn codex_completed_item_text(value: &serde_json::Value) -> Option<String> {
    let item = value.get("item")?;
    if item.get("type").and_then(serde_json::Value::as_str) != Some("agent_message") {
        return None;
    }
    parse_text_payload(&item.to_string())
}

fn parse_codex_output(output: &str) -> Result<String> {
    let output = output.trim();
    if output.is_empty() {
        return Err(NeuralError::MalformedPayload {
            value: "empty codex output".to_string(),
            expected: "non-empty Codex response".to_string(),
        });
    }
    Ok(parse_text_payload(output).unwrap_or_else(|| output.to_string()))
}

fn codex_session_id_from_diagnostics(value: &str) -> Option<String> {
    value
        .lines()
        .filter_map(codex_session_id_from_line)
        .next_back()
}

fn codex_session_id_from_line(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    codex_thread_id_from_value(&value)
}

fn codex_thread_id_from_value(value: &serde_json::Value) -> Option<String> {
    let kind = value.get("type").and_then(serde_json::Value::as_str)?;
    let field = match kind {
        "thread.started" => "thread_id",
        "session_meta" => "id",
        _ => return None,
    };
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn codex_error_message(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(serde_json::Value::as_str) != Some("error") {
        return None;
    }
    value
        .get("message")
        .or_else(|| value.pointer("/error/message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn response(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    output: CodexTurnOutput,
    hot: bool,
) -> LlmResponse {
    LlmResponse {
        provider_id: config.provider_id.clone(),
        model: selected_codex_model(config, request)
            .unwrap_or(CODEX_PROVIDER_DEFAULT_MODEL)
            .to_string(),
        content: output.content,
        metadata: codex_response_metadata(hot, output.provider_session_id),
    }
}

fn codex_response_metadata(hot: bool, provider_session_id: Option<String>) -> serde_json::Value {
    let mut metadata = json!({ "backend": "codex-cli", "hot": hot, "transport": "client" });
    if let (Some(object), Some(session_id)) = (metadata.as_object_mut(), provider_session_id) {
        object.insert("provider_session_id".to_string(), json!(session_id));
    }
    metadata
}

fn codex_client_config_args(config: &LlmProviderConfig, request: &LlmRequest) -> Vec<String> {
    let mut args = vec!["-c".into(), APP_OWNED_CODEX_NOTIFY.into()];
    if let Some(model) = selected_codex_model(config, request) {
        args.extend(model_args(model));
    }
    args
}

fn codex_mcp_server_args(request: &LlmRequest) -> Vec<String> {
    let requested = request
        .mcp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect::<BTreeSet<_>>();
    let disabled = load_codex_active_mcp_server_names()
        .into_iter()
        .filter(|name| !requested.contains(name.as_str()))
        .map(|name| format!("mcp_servers.{name}.enabled=false"))
        .flat_map(|entry| ["-c".to_string(), entry]);
    disabled
        .chain(
            request
                .mcp_servers
                .iter()
                .flat_map(codex_mcp_server_config_args),
        )
        .collect()
}

fn codex_mcp_server_config_args(server: &crate::llm_providers::LlmMcpServerConfig) -> Vec<String> {
    let base = format!("mcp_servers.{}", server.name);
    vec![
        format!("{base}.enabled=true"),
        format!("{base}.url={}", toml_string(&mcp_message_url(&server.url))),
        format!(
            "{base}.default_tools_approval_mode={}",
            toml_string(codex_scoped_tool_approval(server))
        ),
    ]
    .into_iter()
    .flat_map(|entry| ["-c".to_string(), entry])
    .collect()
}

fn load_codex_active_mcp_server_names() -> Vec<String> {
    let Some(config_path) = resolve_codex_config_path() else {
        return Vec::new();
    };
    let Ok(config) = std::fs::read_to_string(config_path) else {
        return Vec::new();
    };
    parse_codex_active_mcp_servers(&config)
}

fn resolve_codex_config_path() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME")
        && !codex_home.is_empty()
    {
        return Some(PathBuf::from(codex_home).join("config.toml"));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".codex").join("config.toml"))
}

fn parse_codex_active_mcp_servers(config: &str) -> Vec<String> {
    let mut servers = Vec::new();
    let mut current_server = None;
    for raw_line in config.lines() {
        let line = strip_toml_comment(raw_line);
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_server = parse_codex_mcp_header(trimmed);
            if let Some(name) = &current_server {
                servers.push((name.clone(), true));
            }
            continue;
        }
        if trimmed.starts_with("enabled")
            && trimmed.contains("false")
            && let Some(name) = current_server.as_deref()
            && let Some((_, enabled)) = servers.iter_mut().find(|(server, _)| server == name)
        {
            *enabled = false;
        }
    }
    servers
        .into_iter()
        .filter_map(|(name, enabled)| {
            (enabled && codex_bare_config_key_segment(&name)).then_some(name)
        })
        .collect()
}

fn parse_codex_mcp_header(trimmed: &str) -> Option<String> {
    let section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
    let rest = section.strip_prefix("mcp_servers.")?;
    if rest.contains('.') || rest.is_empty() {
        return None;
    }
    Some(rest.trim_matches('"').to_string())
}

fn strip_toml_comment(line: &str) -> String {
    let mut in_string = false;
    let mut escaped = false;
    let mut result = String::new();
    for ch in line.chars() {
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '#' {
            break;
        }
        if ch == '"' {
            in_string = true;
        }
        result.push(ch);
    }
    result
}

fn codex_bare_config_key_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn codex_mcp_server_urls(request: &LlmRequest) -> Vec<String> {
    request
        .mcp_servers
        .iter()
        .map(|server| format!("{}={}", server.name, server.url))
        .collect()
}

fn codex_mcp_stream_key(request: &LlmRequest) -> String {
    codex_mcp_server_urls(request).join("|")
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn toml_key_segment(value: &str) -> String {
    if codex_bare_config_key_segment(value) {
        return value.to_string();
    }
    toml_string(value)
}

fn codex_model_argument(model: &str) -> Option<&str> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed == CODEX_PROVIDER_DEFAULT_MODEL {
        return None;
    }
    Some(trimmed)
}

fn selected_codex_model<'a>(
    config: &'a LlmProviderConfig,
    request: &'a LlmRequest,
) -> Option<&'a str> {
    request
        .model_id()
        .and_then(codex_model_argument)
        .or_else(|| codex_model_argument(&config.model))
}

mod process_support;
use process_support::{
    decorate_hot_error, hot_closed, missing_pipe, missing_spawn, shell_quote, unique_output_path,
    write_prompt_file,
};

#[cfg(test)]
mod tests;
