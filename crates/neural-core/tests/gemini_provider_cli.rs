use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{
    LlmMcpServerConfig, LlmMessage, LlmRequest, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tempfile::TempDir;

#[derive(Clone)]
struct UnusedFakeHttpClient;

impl LlmHttpClient for UnusedFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!(
            "Gemini CLI provider should not post JSON to `{}`",
            request.endpoint
        )
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!(
            "Gemini CLI provider should not stream text from `{}`",
            request.endpoint
        )
    }
}

#[test]
fn gemini_provider_uses_cli_plan_mode_args_and_extracts_response_text() {
    let temp = TempDir::new().unwrap();
    let args_path = temp.path().join("gemini-args.txt");
    let script = fake_script(
        &temp,
        "fake-gemini",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf '{{\"response\":\" gemini text \"}}'",
            args_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![gemini_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let response = registry
        .complete("gemini", &request("build a plan"))
        .unwrap();
    let args = fs::read_to_string(args_path).unwrap();
    let arg_lines = args.lines().collect::<Vec<_>>();

    assert_eq!(response.content, "gemini text");
    assert_eq!(response.provider_id, "gemini");
    assert_eq!(response.model, "flash");
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--prompt", "user: build a plan"])
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--output-format", "json"])
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--approval-mode", "plan"])
    );
    assert!(arg_lines.contains(&"--sandbox"));
    assert!(!arg_lines.contains(&"--allowed-mcp-server-names"));
    assert!(!arg_lines.contains(&"--extensions"));
}

#[test]
fn gemini_provider_auto_approves_scoped_mcp_tools_without_stalling() {
    let temp = TempDir::new().unwrap();
    let args_path = temp.path().join("gemini-mcp-args.txt");
    let settings_path = temp.path().join("gemini-mcp-settings.json");
    let script = fake_script(
        &temp,
        "fake-gemini-mcp",
        &format!(
            r#"printf '%s\n' "$@" > '{}'
approval_mode=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--approval-mode' ]; then approval_mode="$arg"; fi
  previous="$arg"
done
if [ "$approval_mode" != 'yolo' ]; then
  printf 'headless MCP requires yolo approval\n' >&2
  exit 91
fi
cat .gemini/settings.json > '{}'
printf '{{"response":"mcp reply"}}'"#,
            args_path.display(),
            settings_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![gemini_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let response = registry
        .complete("gemini", &request_with_mcp("update canvas"))
        .unwrap();
    let args = fs::read_to_string(args_path).unwrap();
    let arg_lines = args.lines().collect::<Vec<_>>();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();

    assert_eq!(response.content, "mcp reply");
    assert_eq!(
        settings["mcpServers"]["lumvise-assistant"]["url"],
        "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a"
    );
    assert_eq!(settings["mcpServers"]["lumvise-assistant"]["trust"], true);
    assert!(
        settings["mcpServers"]["lumvise-assistant"]
            .get("httpUrl")
            .is_none()
    );
    assert_eq!(
        settings["mcp"]["allowed"],
        serde_json::json!(["lumvise-assistant"])
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--approval-mode", "yolo"])
    );
    assert!(!arg_lines.contains(&"default"));
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--allowed-mcp-server-names", "lumvise-assistant"])
    );
    assert!(!arg_lines.iter().any(|arg| arg.starts_with("__lumvise_")));
}

#[test]
fn gemini_provider_reports_malformed_and_missing_response_output() {
    let malformed_error = gemini_error("fake-malformed-gemini", "printf 'not json'");
    let missing_error = gemini_error("fake-missing-gemini", "printf '{\"result\":\"done\"}'");

    assert!(malformed_error.contains("expected Gemini JSON stdout"));
    assert!(missing_error.contains("expected Gemini JSON with non-empty `response`"));
}

#[test]
fn gemini_provider_resumes_existing_session_id() {
    let temp = TempDir::new().unwrap();
    let args_path = temp.path().join("gemini-resume-args.txt");
    let script = fake_script(
        &temp,
        "fake-gemini-resume",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf '{{\"response\":\"resumed\"}}'",
            args_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![gemini_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();
    let mut request = request("continue");
    request.provider_session_id = Some("c95ea3ad-f4ff-4403-b76a-e822e1c4ee44".to_string());

    let response = registry.complete("gemini", &request).unwrap();
    let args = fs::read_to_string(args_path).unwrap();

    assert_eq!(response.content, "resumed");
    assert_eq!(
        response.metadata["provider_session_id"],
        serde_json::json!("c95ea3ad-f4ff-4403-b76a-e822e1c4ee44")
    );
    assert!(args.contains("--resume\nc95ea3ad-f4ff-4403-b76a-e822e1c4ee44"));
}

#[test]
fn gemini_provider_discovers_new_session_for_named_conversation() {
    let temp = TempDir::new().unwrap();
    let script = fake_script(
        &temp,
        "fake-gemini-list-session",
        r#"if [ "$1" = "--list-sessions" ]; then
  printf '%s\n' 'Available sessions for this project (1):'
  printf '%s\n' '  1. Assistant turn [aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee]'
  exit 0
fi
printf '{"response":"first"}'"#,
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![gemini_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();
    let mut request = request("start");
    request.conversation_id = Some("assistant-session".to_string());

    let response = registry.complete("gemini", &request).unwrap();

    assert_eq!(response.content, "first");
    assert_eq!(
        response.metadata["provider_session_id"],
        serde_json::json!("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
    );
}

#[test]
fn gemini_provider_streams_json_lines_as_content_deltas() {
    let temp = TempDir::new().unwrap();
    let script = fake_script(
        &temp,
        "fake-streaming-gemini",
        r#"printf '{"type":"message","content":"hel"}\n'
printf '{"type":"message","content":"lo"}\n'
printf '{"type":"result","response":"hello"}\n'"#,
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![gemini_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let events = registry
        .stream("gemini", &request("stream"), StreamControl::unbounded())
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "hel".to_string()
            },
            LlmStreamEvent::ContentDelta {
                text: "lo".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
}

fn gemini_config(command: String) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "gemini".to_string(),
        kind: LlmProviderKind::Gemini,
        model: "flash".to_string(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command,
            args: vec![],
            timeout_ms: 1000,
        }),
    }
}

fn request(content: &str) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: content.to_string(),
        }],
        stream: false,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

fn request_with_mcp(content: &str) -> LlmRequest {
    let mut request = request(content);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".to_string(),
        url: "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a".to_string(),
    }];
    request
}

fn fake_script(temp: &TempDir, name: &str, body: &str) -> String {
    let path = temp.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path.to_string_lossy().to_string()
}

fn gemini_error(name: &str, body: &str) -> String {
    let temp = TempDir::new().unwrap();
    let registry = LlmProviderRegistry::from_configs(
        vec![gemini_config(fake_script(&temp, name, body))],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    registry
        .complete("gemini", &request("build a plan"))
        .unwrap_err()
        .to_string()
}
