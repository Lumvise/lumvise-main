use super::*;
use crate::config::{LlmProviderKind, SpawnConfig};
use crate::llm_providers::contract::{LlmHttpRequest, LlmProvider};
use crate::llm_providers::{LlmMcpServerConfig, LlmMessage};
use crate::process::StreamControl;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn app_owned_codex_commands_disable_recursive_completion_notifications() {
    let config = test_config(String::new());
    let request = test_request();
    for args in [
        codex_stateless_args(&config, &request, Path::new("output")),
        codex_app_server_process_args(&config, &request),
    ] {
        assert!(args.windows(2).any(|pair| pair == ["-c", "notify=[]"]));
    }
    let script = hot_shell_script("codex", &config);
    assert_eq!(script.matches("-c 'notify=[]'").count(), 2);
}

#[derive(Debug)]
struct NoHttpClient;

impl LlmHttpClient for NoHttpClient {
    fn post_json(&self, _: &LlmHttpRequest) -> Result<serde_json::Value> {
        unreachable!("Codex CLI tests do not use HTTP")
    }

    fn stream_text(&self, _: &LlmHttpRequest) -> Result<Vec<String>> {
        unreachable!("Codex CLI tests do not use HTTP")
    }
}

#[test]
fn stateless_stream_returns_when_descendant_keeps_stdout_open() {
    let provider = CodexProvider::new(
        test_config(stdout_inheritance_script()),
        Arc::new(NoHttpClient),
    )
    .expect("codex test provider");
    let started_at = Instant::now();

    let events = provider
        .stream(&test_request(), StreamControl::unbounded())
        .expect("stream events");

    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert!(events.contains(&LlmStreamEvent::FinalText { text: "OK".into() }));
    assert_eq!(events.last(), Some(&LlmStreamEvent::Complete));
}

#[test]
fn parse_active_codex_mcp_servers_ignores_disabled_and_nested_sections() {
    let config = r#"
[mcp_servers.context7]
enabled = true

[mcp_servers.lumvise-assistant]
url = "http://127.0.0.1"

[mcp_servers.disabled-one]
enabled = false

[mcp_servers.context7.tools.lookup]
approval_mode = "approve"
"#;

    let servers = parse_codex_active_mcp_servers(config);

    assert_eq!(
        servers,
        vec!["context7".to_string(), "lumvise-assistant".to_string()]
    );
}

#[test]
fn isolated_codex_config_strips_mcp_server_tables() {
    let config = r#"
model = "gpt-5"
model_provider = "openai"

[mcp_servers.context7]
command = "npx"

[mcp_servers.context7.env]
TOKEN = "secret"

[profiles.assistant]
model = "gpt-5.4"
"#;

    let sanitized = sanitize_codex_config_without_mcp_servers(config);

    assert!(sanitized.contains("[profiles.assistant]"));
    assert!(sanitized.contains("model = \"gpt-5.4\""));
    assert!(!sanitized.contains("model = \"gpt-5\"\n"));
    assert!(!sanitized.contains("[mcp_servers"));
    assert!(!sanitized.contains("TOKEN ="));
    assert!(!sanitized.contains("model_provider ="));
}

#[test]
fn isolated_codex_config_writes_requested_assistant_mcp_server_tables() {
    let mut request = test_request();
    request.mcp_servers[0].name = "lumvise-assistant".to_string();
    request.mcp_servers[0].url =
        "http://127.0.0.1:4180/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/session-a".to_string();

    let config = codex_mcp_config_toml(&request);

    assert!(config.contains("[mcp_servers.lumvise-assistant]"));
    assert!(config.contains("enabled = true"));
    assert!(
        config.contains("url = \"http://127.0.0.1:4180/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/session-a\"")
    );
    // Server-level approval covers each tool in the trusted session scope;
    // an incomplete per-tool list previously stranded headless callers.
    assert!(config.contains("default_tools_approval_mode = \"approve\""));
    assert!(!config.contains(".tools."));
}

#[test]
fn isolated_codex_config_omits_mcp_tables_without_scoped_servers() {
    let mut request = test_request();
    request.mcp_servers.clear();

    assert!(codex_mcp_config_toml(&request).is_empty());
}

struct FakeCodexApprovalProbe;

struct FakeCodexToolOnlyCompletion;

impl FakeCodexToolOnlyCompletion {
    fn provider() -> CodexProvider {
        let script = r#"while [ "$#" -gt 0 ]; do
            if [ "$1" = '-o' ]; then shift; : > "$1"; fi; shift; done
            printf '%s\n' '{"type":"thread.started","thread_id":"tool-only-session"}'"#;
        CodexProvider::new(test_config(script.into()), Arc::new(NoHttpClient))
            .expect("tool-only provider")
    }
}

#[test]
fn tool_only_completion_keeps_provider_session_without_inventing_spoken_text() {
    let response = FakeCodexToolOnlyCompletion::provider()
        .complete(&test_request())
        .expect("tool completion");
    assert_eq!(response.content, "");
    assert_eq!(
        response.metadata["provider_session_id"],
        "tool-only-session"
    );
}

#[test]
fn empty_plain_completion_still_fails() {
    let mut request = test_request();
    request.mcp_servers.clear();
    request.model = Some("test-model".into());
    assert!(
        FakeCodexToolOnlyCompletion::provider()
            .complete(&request)
            .is_err()
    );
}

impl FakeCodexApprovalProbe {
    fn provider() -> CodexProvider {
        let script = r#"out=''; policy=''; while [ "$#" -gt 0 ]; do case "$1" in
            -o) shift; out="$1";;
            mcp_servers.*.default_tools_approval_mode=*) policy="$1";;
            esac; shift; done; printf '%s' "$policy" > "$out""#;
        CodexProvider::new(test_config(script.into()), Arc::new(NoHttpClient))
            .expect("approval probe provider")
    }
}

#[test]
fn public_provider_approves_only_the_trusted_assistant_scope() {
    let mut request = test_request();
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".into(),
        url: "http://127.0.0.1:4180/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/session-a".into(),
    }];
    let response = FakeCodexApprovalProbe::provider()
        .complete(&request)
        .expect("completion");
    assert_eq!(
        response.content,
        "mcp_servers.lumvise-assistant.default_tools_approval_mode=\"approve\""
    );
}

#[test]
fn assistant_endpoint_with_an_unrelated_server_name_keeps_automatic_review() {
    let mut request = test_request();
    request.mcp_servers[0].url = "http://127.0.0.1:4180/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/session-a".into();
    let response = FakeCodexApprovalProbe::provider()
        .complete(&request)
        .expect("completion");
    assert!(response.content.ends_with("=\"auto\""));
    assert!(codex_mcp_config_toml(&request).contains("default_tools_approval_mode = \"auto\""));
}

#[test]
fn unrelated_mcp_endpoints_keep_automatic_review_in_both_launch_paths() {
    let endpoints = [
        "https://example.com/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/session-a",
        "http://127.0.0.1:4180/mcp",
        "http://127.0.0.1:4180/api/scoped-plugin-mcp/messages/assistant_session/builtin.assistant/",
        "invalid-url",
    ];
    for endpoint in endpoints {
        let mut request = test_request();
        request.mcp_servers[0] = LlmMcpServerConfig {
            name: "lumvise-assistant".into(),
            url: endpoint.into(),
        };
        let response = FakeCodexApprovalProbe::provider()
            .complete(&request)
            .expect("completion");
        assert!(
            response.content.ends_with("=\"auto\""),
            "{endpoint}: {}",
            response.content
        );
        assert!(
            codex_mcp_config_toml(&request).contains("default_tools_approval_mode = \"auto\""),
            "{endpoint}"
        );
    }
}

fn test_config(script: String) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "codex".into(),
        kind: LlmProviderKind::Codex,
        model: CODEX_PROVIDER_DEFAULT_MODEL.into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "sh".into(),
            args: vec!["-c".into(), script, "codex-shim".into()],
            timeout_ms: 5_000,
        }),
    }
}

fn test_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: "hello".into(),
        }],
        stream: true,
        provider_id: Some("codex".into()),
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: vec![LlmMcpServerConfig {
            name: "test".into(),
            url: "http://127.0.0.1/mcp".into(),
        }],
        modality_inputs: Vec::new(),
    }
}

fn stdout_inheritance_script() -> String {
    "output_path=''; while [ \"$#\" -gt 0 ]; do if [ \"$1\" = '-o' ]; then shift; output_path=\"$1\"; fi; shift || break; done; printf 'OK' > \"$output_path\"; sleep 2 & exit 0".into()
}

#[test]
fn codex_home_precedence_and_platform_fallback() {
    for (codex, home, platform, expected) in [
        (Some("c"), Some("h"), Some("p"), Some("c")),
        (None, Some("h"), Some("p"), Some("h/.codex")),
        (Some(""), Some(""), Some("p"), Some("p/.codex")),
        (None, None, Some("p"), Some("p/.codex")),
        (None, None, None, None),
    ] {
        assert_eq!(
            codex_home_from(
                codex.map(Into::into),
                home.map(Into::into),
                platform.map(PathBuf::from)
            ),
            expected.map(PathBuf::from)
        );
    }
}

#[test]
fn codex_config_path_uses_the_resolved_home() {
    let home = resolve_existing_codex_home();
    let config = resolve_codex_config_path();
    assert_eq!(config, home.map(|path| path.join("config.toml")));
}

struct CancelledCodexCall;

impl ProviderCallControl for CancelledCodexCall {
    fn remaining(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn native_codex_scoped_completion_keeps_app_server_dispatch() {
    for command in native_codex_commands() {
        let provider = native_codex_test_provider(command);
        let mut request = test_request();
        request.model = Some("request-model".into());
        assert!(provider.hot_enabled());
        assert!(matches!(
            provider.run_turn(&request, Some(&CancelledCodexCall)),
            Err(NeuralError::ProcessCancelled { .. })
        ));
        assert_eq!(provider.app_server_sessions.lock().unwrap().len(), 1);
        assert!(provider.hot_session.lock().unwrap().is_none());
    }
}

fn native_codex_commands() -> &'static [&'static str] {
    if cfg!(windows) {
        return &[
            "codex",
            "codex.exe",
            "CODEX.EXE",
            r"C:\missing\bin\codex.exe",
        ];
    }
    &["codex", "/missing/bin/codex"]
}

fn native_codex_test_provider(command: &str) -> CodexProvider {
    let mut config = test_config(String::new());
    config.spawn.as_mut().unwrap().command = command.into();
    CodexProvider::new(config, Arc::new(NoHttpClient)).unwrap()
}

#[test]
fn native_codex_recognition_does_not_include_cmd_wrappers() {
    for (command, expected) in [
        ("codex", true),
        ("codex.exe", cfg!(windows)),
        ("codex.cmd", false),
    ] {
        let provider = native_codex_test_provider(command);
        assert_eq!(provider.hot_enabled(), expected, "{command}");
    }
}

#[test]
fn native_codex_scoped_stream_keeps_app_server_dispatch() {
    let provider = native_codex_test_provider(native_codex_commands()[0]);
    let control = StreamControl::unbounded()
        .with_cancel_flag(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    assert!(matches!(
        provider.stream_with_events(&test_request(), control, &mut |_| Ok(())),
        Err(NeuralError::ProcessCancelled { .. })
    ));
    assert_eq!(provider.app_server_sessions.lock().unwrap().len(), 1);
    assert!(provider.hot_session.lock().unwrap().is_none());
}

#[cfg(windows)]
#[test]
fn windows_plain_codex_completion_uses_native_process_without_shell() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["codex", "codex.exe"] {
        let provider = native_codex_test_provider(directory.path().join(name).to_str().unwrap());
        let mut request = test_request();
        request.mcp_servers.clear();
        assert!(
            matches!(provider.complete(&request), Err(NeuralError::Io { expected, .. }) if expected == "provider command process")
        );
        assert!(provider.hot_session.lock().unwrap().is_none());
        assert!(provider.app_server_sessions.lock().unwrap().is_empty());
    }
}

#[cfg(unix)]
struct FakeCodexArgumentRecorder {
    directory: tempfile::TempDir,
    executable: PathBuf,
    record: PathBuf,
}

#[cfg(unix)]
impl FakeCodexArgumentRecorder {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::Builder::new()
            .prefix("codex fixture's ")
            .tempdir()
            .unwrap();
        let executable = directory.path().join(name);
        let record = directory.path().join("arguments");
        std::fs::write(&executable, Self::script()).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            executable,
            record,
        }
    }

    fn script() -> &'static str {
        r#"#!/bin/sh
record="$1"; shift
printf '%s\000' "$@" > "$record"
out=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '-o' ]; then shift; out="$1"; fi
  shift
done
printf 'fixture answer' > "$out"
printf '%s\n' '{"type":"session_meta","id":"fixture-session"}'
"#
    }

    fn config(&self) -> LlmProviderConfig {
        let mut config = test_config(String::new());
        config.model = "configured-model".into();
        config.spawn.as_mut().unwrap().command = self.executable.to_string_lossy().into();
        config.spawn.as_mut().unwrap().args = vec![self.record.to_string_lossy().into()];
        config
    }

    fn arguments(&self) -> Vec<String> {
        std::fs::read(&self.record)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
            .collect()
    }

    fn shell_turn(&self, shell: &str, session: &str, prompt: &str) -> std::process::Output {
        use std::io::Write;
        let prompt_path = self.directory.path().join("prompt file");
        std::fs::write(&prompt_path, prompt).unwrap();
        let mut child = self.shell_process(shell);
        let request = format!("marker\t{session}\t{}\n", prompt_path.display());
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn shell_process(&self, shell: &str) -> std::process::Child {
        use std::process::{Command, Stdio};
        let script = hot_shell_script(self.executable.to_str().unwrap(), &self.config());
        Command::new(shell)
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn assert_shell_turn(&self, shell: &str, session: &str, prompt: &str) {
        let output = self.shell_turn(shell, session, prompt);
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "lumvise-codex-session-id\tfixture-session\nfixture answer\nmarker\n"
        );
        self.assert_hot_arguments(session, prompt);
    }

    fn assert_hot_arguments(&self, session: &str, prompt: &str) {
        let args = self.arguments();
        assert_eq!(args.last().unwrap(), prompt);
        self.assert_argument_pairs(&[["--model", "configured-model"]]);
        assert_eq!(
            args.get(1).is_some_and(|arg| arg == "resume"),
            session != CODEX_NO_SESSION
        );
        if session != CODEX_NO_SESSION {
            assert_eq!(args[args.len() - 2], session);
        }
    }

    fn assert_argument_pairs(&self, expected: &[[&str; 2]]) {
        let args = self.arguments();
        for pair in expected {
            assert!(
                args.windows(2).any(|window| window == *pair),
                "{pair:?}: {args:?}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn hot_shell_round_trip_is_posix_and_preserves_inputs() {
    let fixture = FakeCodexArgumentRecorder::new("codex");
    let prompt = "user: literal $HOME $(echo injected) 'quote'\t tab\nsecond line";
    for shell in ["/bin/sh", "/bin/dash"]
        .into_iter()
        .filter(|shell| Path::new(shell).exists())
    {
        for session in [CODEX_NO_SESSION, "resume '$literal"] {
            fixture.assert_shell_turn(shell, session, prompt);
        }
    }
}

#[cfg(unix)]
#[test]
fn unix_plain_codex_completion_uses_hot_shell() {
    let fixture = FakeCodexArgumentRecorder::new("codex");
    let provider = CodexProvider::new(fixture.config(), Arc::new(NoHttpClient)).unwrap();
    let mut request = test_request();
    request.mcp_servers.clear();
    request.provider_session_id = Some("resume-session".into());
    let response = provider.complete(&request).unwrap();
    assert_eq!(response.content, "fixture answer");
    assert_eq!(response.metadata["provider_session_id"], "fixture-session");
    assert!(provider.hot_session.lock().unwrap().is_some());
    assert_eq!(fixture.arguments().last().unwrap(), &prompt_text(&request));
}

#[cfg(unix)]
#[test]
fn stateless_codex_completion_preserves_model_session_prompt_and_mcp() {
    let fixture = FakeCodexArgumentRecorder::new("codex-fixture");
    let provider = CodexProvider::new(fixture.config(), Arc::new(NoHttpClient)).unwrap();
    let mut request = test_request();
    request.model = Some("request-model".into());
    request.provider_session_id = Some("resume-session".into());
    request.messages[0].content = "literal '$HOME'\t tab\nsecond line".into();
    assert_eq!(
        provider.complete(&request).unwrap().content,
        "fixture answer"
    );
    fixture.assert_argument_pairs(&[
        ["--model", "request-model"],
        ["--sandbox", "read-only"],
        ["resume", "resume-session"],
        ["-c", "mcp_servers.test.url=\"http://127.0.0.1/mcp\""],
    ]);
    assert_eq!(fixture.arguments().last().unwrap(), &prompt_text(&request));
    assert!(provider.hot_session.lock().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn explicit_model_keeps_native_codex_on_stateless_dispatch() {
    let fixture = FakeCodexArgumentRecorder::new("codex");
    let provider = CodexProvider::new(fixture.config(), Arc::new(NoHttpClient)).unwrap();
    let mut request = test_request();
    request.model = Some("request-model".into());
    request.mcp_servers.clear();
    assert_eq!(
        provider.complete(&request).unwrap().content,
        "fixture answer"
    );
    fixture.assert_argument_pairs(&[["--model", "request-model"], ["--sandbox", "read-only"]]);
    assert!(provider.hot_session.lock().unwrap().is_none());
}
