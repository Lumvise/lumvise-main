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
