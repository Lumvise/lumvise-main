//! Per-client project execution provider owned by the unified `lumvise mcp` role.
//!
//! The desktop App Runtime owns job state and dispatch. This module only registers
//! one project-scoped provider, executes the fixed semantic-artifact capability,
//! and publishes progress/results through the credentialed App Bridge.

use crate::AppBridgeConfig;
use lumvise_app_core::SemanticArtifactTask;
use lumvise_mcp_core::app_bridge_transport::{AppBridgeHttpMethod, AppBridgeHttpTransport};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

const CAPABILITY: &str = "semantic.generate_functional_artifacts.v1";
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const EXECUTION_DEADLINE: Duration = Duration::from_secs(60);
const LEASE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

const REGISTER_ENDPOINT: &str = "/api/project-execution/providers/register";
const NEXT_ENDPOINT: &str = "/api/project-execution/providers/next";
const PROGRESS_ENDPOINT: &str = "/api/project-execution/providers/progress";
const RESULT_ENDPOINT: &str = "/api/project-execution/providers/result";
const HEARTBEAT_ENDPOINT: &str = "/api/project-execution/providers/heartbeat";
const UNREGISTER_ENDPOINT: &str = "/api/project-execution/providers/unregister";

/// Configuration for one project provider attached to a per-client MCP broker.
#[derive(Clone, Debug)]
pub struct ProjectExecutionConfig {
    app_bridge: AppBridgeConfig,
    provider_id: String,
    project_root: PathBuf,
    native_llm_engine: Option<String>,
}

impl ProjectExecutionConfig {
    /// Creates a provider for one exact project root.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use lumvise_mcp_app_adapter::{AppBridgeConfig, ProjectExecutionConfig};
    /// let _config = ProjectExecutionConfig::new(
    ///     AppBridgeConfig::discovery(),
    ///     "mcp-client-1",
    ///     ".",
    /// );
    /// ```
    pub fn new(
        app_bridge: AppBridgeConfig,
        provider_id: impl Into<String>,
        project_root: impl AsRef<Path>,
    ) -> Self {
        Self {
            app_bridge,
            provider_id: provider_id.into(),
            project_root: project_root.as_ref().to_path_buf(),
            native_llm_engine: None,
        }
    }

    /// Selects the local read-only LLM engine used by the fixed capability.
    pub fn with_native_llm_engine(mut self, engine: impl Into<String>) -> Self {
        self.native_llm_engine = Some(engine.into());
        self
    }

    fn validate(&self) -> Result<(), String> {
        if self.provider_id.trim().is_empty() {
            return Err("invalid project provider id ``; expected non-empty text".into());
        }
        if !self.project_root.is_dir() {
            return Err(format!(
                "invalid project root `{}`; expected an existing directory",
                self.project_root.display()
            ));
        }
        Ok(())
    }
}

/// Running project provider. Dropping it unregisters and joins the worker.
pub struct ProjectExecutionProvider {
    shutdown: watch::Sender<bool>,
    worker: Option<JoinHandle<()>>,
}

impl ProjectExecutionProvider {
    /// Starts the provider worker and returns once the worker owns its configuration.
    pub fn spawn(config: ProjectExecutionConfig) -> Result<Self, String> {
        config.validate()?;
        let (shutdown, receiver) = watch::channel(false);
        let worker = std::thread::Builder::new()
            .name("lumvise-mcp-project-execution".into())
            .spawn(move || run_worker(config, receiver))
            .map_err(|error| format!("failed to spawn project execution provider: {error}"))?;
        Ok(Self {
            shutdown,
            worker: Some(worker),
        })
    }
}

impl Drop for ProjectExecutionProvider {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(config: ProjectExecutionConfig, shutdown: watch::Receiver<bool>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "failed to build project execution runtime");
            return;
        }
    };
    runtime.block_on(run_provider(config, shutdown));
}

async fn run_provider(config: ProjectExecutionConfig, mut shutdown: watch::Receiver<bool>) {
    let client = AppBridgeHttpTransport::new();
    loop {
        if *shutdown.borrow() || config.app_bridge.is_terminal() {
            return;
        }
        if let Err(error) = run_connected(&client, &config, &mut shutdown).await {
            if config.app_bridge.is_terminal() {
                tracing::info!(
                    provider_id = %config.provider_id,
                    "project execution provider stopping after terminal runtime shutdown"
                );
                return;
            }
            tracing::warn!(%error, provider_id = %config.provider_id, "project execution provider reconnecting");
        }
        if wait_or_shutdown(&mut shutdown, RECONNECT_DELAY).await {
            return;
        }
    }
}

async fn run_connected(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let connection: ProviderConnection = post_json(
        client,
        config,
        REGISTER_ENDPOINT,
        &RegisterRequest {
            provider_id: &config.provider_id,
            project_root: &config.project_root.to_string_lossy(),
            capabilities: [CAPABILITY],
        },
    )
    .await?;
    tracing::info!(provider_id = %config.provider_id, project_root = %config.project_root.display(), "registered project execution provider");

    loop {
        let request = AuthenticatedRequest {
            provider_id: &config.provider_id,
            connection_token: &connection.connection_token,
            timeout_ms: 2_000,
        };
        let next = post_json::<_, NextResponse>(client, config, NEXT_ENDPOINT, &request);
        let response = tokio::select! {
            response = next => response?,
            _ = shutdown.changed() => break,
        };
        let Some(command) = response.command else {
            continue;
        };
        execute_command(client, config, &connection, command).await?;
    }

    let _ = post_json::<_, Value>(
        client,
        config,
        UNREGISTER_ENDPOINT,
        &ProviderAuthentication {
            provider_id: &config.provider_id,
            connection_token: &connection.connection_token,
        },
    )
    .await;
    Ok(())
}

async fn execute_command(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    connection: &ProviderConnection,
    command: ProjectExecutionCommand,
) -> Result<(), String> {
    match command {
        ProjectExecutionCommand::Execute {
            job_id,
            capability_id,
            input,
        } if capability_id == CAPABILITY => {
            post_progress(client, config, connection, &job_id).await?;
            let result = execute_with_heartbeat(client, config, connection, input).await?;
            post_result(client, config, connection, &job_id, result).await
        }
        ProjectExecutionCommand::Execute {
            job_id,
            capability_id,
            ..
        } => {
            post_failure(
                client,
                config,
                connection,
                &job_id,
                format!("unsupported project capability `{capability_id}`"),
            )
            .await
        }
        ProjectExecutionCommand::Cancel { job_id } => {
            tracing::info!(%job_id, "project execution cancellation observed");
            Ok(())
        }
    }
}

async fn execute_with_heartbeat(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    connection: &ProviderConnection,
    input: Value,
) -> Result<NativeCommandResult, String> {
    let execution = execute_semantic_artifacts(
        input,
        &config.project_root,
        config.native_llm_engine.as_deref(),
    );
    tokio::pin!(execution);
    let mut heartbeat = tokio::time::interval(LEASE_HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    loop {
        tokio::select! {
            result = &mut execution => return Ok(result),
            _ = heartbeat.tick() => {
                post_json::<_, Value>(
                    client,
                    config,
                    HEARTBEAT_ENDPOINT,
                    &ProviderAuthentication {
                        provider_id: &config.provider_id,
                        connection_token: &connection.connection_token,
                    },
                ).await?;
            }
        }
    }
}

async fn post_progress(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    connection: &ProviderConnection,
    job_id: &str,
) -> Result<(), String> {
    post_json::<_, Value>(
        client,
        config,
        PROGRESS_ENDPOINT,
        &ProgressRequest {
            provider_id: &config.provider_id,
            connection_token: &connection.connection_token,
            job_id,
            message: "local semantic artifact generation started",
        },
    )
    .await
    .map(|_| ())
}

async fn post_result(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    connection: &ProviderConnection,
    job_id: &str,
    result: NativeCommandResult,
) -> Result<(), String> {
    if result.ok {
        return post_json::<_, Value>(
            client,
            config,
            RESULT_ENDPOINT,
            &ResultRequest {
                provider_id: &config.provider_id,
                connection_token: &connection.connection_token,
                job_id,
                ok: true,
                output: Some(result.output),
                error: None,
            },
        )
        .await
        .map(|_| ());
    }
    let error = result.output["error"]
        .as_str()
        .unwrap_or(&result.summary)
        .to_owned();
    post_failure(client, config, connection, job_id, error).await
}

async fn post_failure(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    connection: &ProviderConnection,
    job_id: &str,
    error: String,
) -> Result<(), String> {
    post_json::<_, Value>(
        client,
        config,
        RESULT_ENDPOINT,
        &ResultRequest {
            provider_id: &config.provider_id,
            connection_token: &connection.connection_token,
            job_id,
            ok: false,
            output: None,
            error: Some(error),
        },
    )
    .await
    .map(|_| ())
}

async fn post_json<T, R>(
    client: &AppBridgeHttpTransport,
    config: &ProjectExecutionConfig,
    endpoint: &str,
    body: &T,
) -> Result<R, String>
where
    T: Serialize + ?Sized,
    R: for<'de> Deserialize<'de>,
{
    let connection = config.app_bridge.current_connection()?;
    let path = format!("{endpoint}?credential={}", connection.app_bridge_credential);
    let body = serde_json::to_value(body)
        .map_err(|error| format!("invalid project execution request for `{endpoint}`: {error}"))?;
    let response = client
        .request_json(
            &connection.app_bridge_base_url,
            AppBridgeHttpMethod::Post,
            &path,
            &body,
        )
        .map_err(|error| {
            format!(
                "project execution endpoint `{}{endpoint}` failed: {error}",
                connection.app_bridge_base_url
            )
        })?;
    serde_json::from_value(response)
        .map_err(|error| format!("invalid project execution response from `{endpoint}`: {error}"))
}

async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = shutdown.changed() => true,
    }
}

struct NativeCommandResult {
    ok: bool,
    summary: String,
    output: Value,
}

async fn execute_semantic_artifacts(
    input: Value,
    project_root: &Path,
    engine_hint: Option<&str>,
) -> NativeCommandResult {
    let request = match SemanticArtifactTask::prepare(input, project_root) {
        Ok(request) => request,
        Err(error) => return invalid_request(error.to_string()),
    };
    let generated = run_native_llm(&request, project_root, engine_hint).await;
    match generated.and_then(|response| request.parse_response(&response)) {
        Ok(output) => NativeCommandResult {
            ok: true,
            summary: format!(
                "generated functional artifacts for `{}`",
                request.target_id()
            ),
            output: json!(output),
        },
        Err(error) => NativeCommandResult {
            ok: false,
            summary: format!(
                "failed to generate functional artifacts for `{}`",
                request.target_id()
            ),
            output: json!({"error": error}),
        },
    }
}

async fn run_native_llm(
    request: &SemanticArtifactTask,
    project_root: &Path,
    engine_hint: Option<&str>,
) -> Result<String, String> {
    let engine = NativeLlmEngine::parse(engine_hint)?;
    let output_path = native_llm_output_path(&request.target_id());
    let prompt = request.prompt();
    let (command, args) = engine.command_and_args(project_root, &output_path, prompt);
    let mut child = Command::new(&command)
        .args(args)
        .current_dir(project_root)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "failed to start native {} LLM `{command}`: {error}",
                engine.name()
            )
        })?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let status = match tokio::time::timeout(EXECUTION_DEADLINE, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            return Err(format!(
                "failed to wait for native {} LLM: {error}",
                engine.name()
            ));
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err(format!(
                "native {} LLM timed out after 60000ms",
                engine.name()
            ));
        }
    };
    let stdout = read_output(&mut stdout).await;
    let stderr = read_output(&mut stderr).await;
    if !status.success() {
        return Err(format!(
            "native {} LLM exited with status {:?}: {}",
            engine.name(),
            status.code(),
            stderr.trim()
        ));
    }
    let file_output = tokio::fs::read_to_string(&output_path)
        .await
        .unwrap_or_default();
    let _ = tokio::fs::remove_file(output_path).await;
    let output = if file_output.trim().is_empty() {
        stdout
    } else {
        file_output
    };
    extract_response(&output)
        .ok_or_else(|| format!("native {} LLM returned no response", engine.name()))
}

async fn read_output(stream: &mut Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    let Some(stream) = stream.as_mut() else {
        return String::new();
    };
    let mut output = String::new();
    let _ = stream.read_to_string(&mut output).await;
    output
}

#[derive(Clone, Copy)]
enum NativeLlmEngine {
    Codex,
    Claude,
    Gemini,
}

impl NativeLlmEngine {
    fn parse(hint: Option<&str>) -> Result<Self, String> {
        let hint = hint
            .map(str::to_owned)
            .or_else(|| std::env::var("LUMVISE_NATIVE_LLM_ENGINE").ok())
            .unwrap_or_else(|| "codex".into());
        match hint
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "codex" | "openai" | "openai_codex" => Ok(Self::Codex),
            "claude" | "anthropic" => Ok(Self::Claude),
            "gemini" | "google" => Ok(Self::Gemini),
            other => Err(format!("unsupported native LLM engine `{other}`")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
        }
    }

    fn command_and_args(
        self,
        project_root: &Path,
        output_path: &Path,
        prompt: String,
    ) -> (String, Vec<String>) {
        match self {
            Self::Codex => (
                command_from_env("LUMVISE_NATIVE_LLM_CODEX_COMMAND", "codex"),
                vec![
                    "exec".into(),
                    "--skip-git-repo-check".into(),
                    "--sandbox".into(),
                    "read-only".into(),
                    "-C".into(),
                    project_root.to_string_lossy().into_owned(),
                    "--json".into(),
                    "-o".into(),
                    output_path.to_string_lossy().into_owned(),
                    prompt,
                ],
            ),
            Self::Claude => (
                command_from_env("LUMVISE_NATIVE_LLM_CLAUDE_COMMAND", "claude"),
                vec![
                    "-p".into(),
                    prompt,
                    "--output-format".into(),
                    "json".into(),
                    "--permission-mode".into(),
                    "plan".into(),
                ],
            ),
            Self::Gemini => (
                command_from_env("LUMVISE_NATIVE_LLM_GEMINI_COMMAND", "gemini"),
                vec![
                    "--prompt".into(),
                    prompt,
                    "--output-format".into(),
                    "json".into(),
                    "--approval-mode".into(),
                    "plan".into(),
                    "--sandbox".into(),
                ],
            ),
        }
    }
}

fn command_from_env(variable: &str, default: &str) -> String {
    std::env::var(variable)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

fn extract_response(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && let Some(response) = response_from_json(&value)
    {
        return Some(response);
    }
    for line in trimmed.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if let Some(response) = response_from_json(&value) {
            return Some(response);
        }
    }
    Some(trimmed.to_owned())
}

fn response_from_json(value: &Value) -> Option<String> {
    [
        "/assistant_response",
        "/response",
        "/result",
        "/final_response",
        "/answer",
        "/content",
        "/message",
        "/output_text",
        "/text",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .map(str::to_owned)
}

fn native_llm_output_path(request_id: &str) -> PathBuf {
    let request_id = request_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    std::env::temp_dir().join(format!(
        "lumvise-semantic-artifacts-{request_id}-{}.json",
        std::process::id()
    ))
}

fn invalid_request(error: String) -> NativeCommandResult {
    NativeCommandResult {
        ok: false,
        summary: "invalid predefined semantic artifact request".into(),
        output: json!({"error": error}),
    }
}

#[derive(Serialize)]
struct RegisterRequest<'a> {
    provider_id: &'a str,
    project_root: &'a str,
    capabilities: [&'a str; 1],
}

#[derive(Deserialize)]
struct ProviderConnection {
    connection_token: String,
}

#[derive(Serialize)]
struct AuthenticatedRequest<'a> {
    provider_id: &'a str,
    connection_token: &'a str,
    timeout_ms: u64,
}

#[derive(Serialize)]
struct ProviderAuthentication<'a> {
    provider_id: &'a str,
    connection_token: &'a str,
}

#[derive(Deserialize)]
struct NextResponse {
    command: Option<ProjectExecutionCommand>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProjectExecutionCommand {
    Execute {
        job_id: String,
        capability_id: String,
        input: Value,
    },
    Cancel {
        job_id: String,
    },
}

#[derive(Serialize)]
struct ProgressRequest<'a> {
    provider_id: &'a str,
    connection_token: &'a str,
    job_id: &'a str,
    message: &'a str,
}

#[derive(Serialize)]
struct ResultRequest<'a> {
    provider_id: &'a str,
    connection_token: &'a str,
    job_id: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
