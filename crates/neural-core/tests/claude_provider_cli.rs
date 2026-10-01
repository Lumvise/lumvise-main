use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{
    LlmMcpServerConfig, LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest,
    LlmStreamEvent,
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
            "Claude CLI provider should not post JSON to `{}`",
            request.endpoint
        )
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!(
            "Claude CLI provider should not stream text from `{}`",
            request.endpoint
        )
    }
}

#[test]
fn claude_provider_uses_cli_plan_mode_args_and_extracts_result_text() {
    let temp = TempDir::new().unwrap();
    let args_path = temp.path().join("claude-args.txt");
    let script = fake_script(
        &temp,
        "fake-claude",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf '{{\"type\":\"result\",\"result\":\" claude text \"}}'",
            args_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let response = registry
        .complete("claude", &request("build a plan"))
        .unwrap();
    let args = fs::read_to_string(args_path).unwrap();
    let arg_lines = args.lines().collect::<Vec<_>>();

    assert_eq!(response.content, "claude text");
    assert_eq!(response.provider_id, "claude");
    assert_eq!(response.model, "sonnet");
    assert!(
        response.metadata["provider_session_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["-p", "user: build a plan"])
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--output-format", "json"])
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "plan"])
    );
    let mcp_config: Value = serde_json::from_str(arg_after(&arg_lines, "--mcp-config")).unwrap();
    assert_eq!(mcp_config, serde_json::json!({"mcpServers": {}}));
    assert!(arg_lines.contains(&"--strict-mcp-config"));
    assert!(!arg_lines.contains(&"--allowedTools"));
}

#[test]
fn claude_provider_resumes_existing_session_id() {
    let temp = TempDir::new().unwrap();
    let args_path = temp.path().join("claude-resume-args.txt");
    let script = fake_script(
        &temp,
        "fake-claude-resume",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf '{{\"type\":\"result\",\"result\":\" resumed \"}}'",
            args_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();
    let mut request = request("continue");
    request.provider_session_id = Some("11111111-2222-4333-8444-555555555555".to_string());

    let response = registry.complete("claude", &request).unwrap();
    let args = fs::read_to_string(args_path).unwrap();

    assert_eq!(response.content, "resumed");
    assert_eq!(
        response.metadata["provider_session_id"],
        serde_json::json!("11111111-2222-4333-8444-555555555555")
    );
    assert!(args.contains("--resume\n11111111-2222-4333-8444-555555555555"));
}

#[test]
fn claude_provider_forwards_assistant_mcp_config_and_allowed_tools() {
    let temp = TempDir::new().unwrap();
    let args_path = temp.path().join("claude-mcp-args.txt");
    let script = fake_script(
        &temp,
        "fake-claude-mcp",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nprintf '{{\"type\":\"result\",\"result\":\"mcp reply\"}}'",
            args_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let response = registry
        .complete("claude", &request_with_mcp("update canvas"))
        .unwrap();
    let args = fs::read_to_string(args_path).unwrap();
    let arg_lines = args.lines().collect::<Vec<_>>();
    let mcp_config = arg_after(&arg_lines, "--mcp-config");
    let mcp_json: Value = serde_json::from_str(mcp_config).unwrap();

    assert_eq!(response.content, "mcp reply");
    assert_eq!(
        mcp_json["mcpServers"]["lumvise-assistant"]["url"],
        "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a"
    );
    assert_eq!(mcp_json["mcpServers"]["lumvise-assistant"]["type"], "http");
    assert!(arg_lines.contains(&"--strict-mcp-config"));
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--allowedTools", "mcp__lumvise-assistant__*"])
    );
    assert!(
        arg_lines
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "default"])
    );
}

#[test]
fn claude_provider_reports_empty_and_missing_result_output() {
    let empty_error = claude_error("fake-empty-claude", "");
    let missing_error = claude_error(
        "fake-missing-claude",
        "printf '{\"type\":\"result\",\"message\":\"done\"}'",
    );

    assert!(empty_error.contains("malformed payload `empty stdout`"));
    assert!(empty_error.contains("expected Claude JSON stdout"));
    assert!(missing_error.contains("expected Claude JSON with non-empty `result`"));
}

#[test]
fn claude_provider_streams_json_lines_as_content_deltas() {
    let temp = TempDir::new().unwrap();
    let script = fake_script(
        &temp,
        "fake-streaming-claude",
        r#"printf '{"type":"content_block_delta","delta":{"type":"text_delta","text":"hel"}}\n'
printf '{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}\n'"#,
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let events = registry
        .stream("claude", &request("stream"), StreamControl::unbounded())
        .unwrap();

    assert!(matches!(
        events.first(),
        Some(LlmStreamEvent::Session { provider_session_id }) if !provider_session_id.is_empty()
    ));
    assert_eq!(
        &events[1..],
        &[
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

#[test]
fn claude_provider_materializes_image_snapshot_for_cli_read_tool() {
    let temp = TempDir::new().unwrap();
    let prompt_path = temp.path().join("claude-prompt.txt");
    let script = fake_script(
        &temp,
        "fake-claude-vision",
        &format!(
            "prompt=\"$2\"\nprintf '%s' \"$prompt\" > '{}'\nimage_path=$(printf '%s' \"$prompt\" | sed -n 's/^  path: //p' | head -n 1)\nif [ ! -f \"$image_path\" ]; then echo \"missing image snapshot file: $image_path\" >&2; exit 7; fi\nprintf '{{\"type\":\"result\",\"result\":\"vision reply\"}}'",
            prompt_path.display()
        ),
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(script)],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let response = registry
        .complete("claude", &image_request("describe the snapshot"))
        .unwrap();
    let prompt = fs::read_to_string(prompt_path).unwrap();

    assert_eq!(response.content, "vision reply");
    assert!(prompt.contains("Image snapshot inputs:"));
    assert!(prompt.contains("id: image-1"));
    assert!(prompt.contains("media_type: image/png"));
    assert!(prompt.contains("Use the Read tool"));
}

#[test]
fn claude_provider_rejects_live_audio_and_screen_frame_inputs() {
    let audio_error = claude_modality_error(LlmModalityInputKind::LiveAudioChunk);
    let frame_error = claude_modality_error(LlmModalityInputKind::ScreenFrame);

    assert!(audio_error.contains("live_audio_input is unsupported"));
    assert!(frame_error.contains("screen_frame_broadcast_input is unsupported"));
}

#[test]
fn claude_provider_rejects_image_snapshot_for_non_image_model() {
    let temp = TempDir::new().unwrap();
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config_with_model(
            fake_script(
                &temp,
                "fake-text-claude",
                "printf '{\"type\":\"result\",\"result\":\"unused\"}'",
            ),
            "claude-instant-1.2",
        )],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    let error = registry
        .complete("claude", &image_request("describe the snapshot"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("claude-instant-1.2"));
    assert!(error.contains("Claude image-capable model"));
}

fn claude_config(command: String) -> LlmProviderConfig {
    claude_config_with_model(command, "sonnet")
}

fn claude_config_with_model(command: String, model: &str) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "claude".to_string(),
        kind: LlmProviderKind::Claude,
        model: model.to_string(),
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

fn image_request(content: &str) -> LlmRequest {
    let mut request = request(content);
    request.modality_inputs = vec![image_snapshot_input()];
    request
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

fn image_snapshot_input() -> LlmModalityInput {
    LlmModalityInput {
        input_id: "image-1".to_string(),
        kind: LlmModalityInputKind::ImageSnapshot,
        media_type: "image/png".to_string(),
        bytes: vec![137, 80, 78, 71],
        metadata: serde_json::json!({ "captured_at": 1 }),
    }
}

fn claude_modality_error(kind: LlmModalityInputKind) -> String {
    let temp = TempDir::new().unwrap();
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(fake_script(
            &temp,
            "fake-unsupported-claude",
            "printf '{\"type\":\"result\",\"result\":\"unused\"}'",
        ))],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();
    let mut request = request("describe the context");
    request.modality_inputs = vec![LlmModalityInput {
        kind,
        ..image_snapshot_input()
    }];

    registry
        .complete("claude", &request)
        .unwrap_err()
        .to_string()
}

fn fake_script(temp: &TempDir, name: &str, body: &str) -> String {
    let path = temp.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path.to_string_lossy().to_string()
}

fn claude_error(name: &str, body: &str) -> String {
    let temp = TempDir::new().unwrap();
    let registry = LlmProviderRegistry::from_configs(
        vec![claude_config(fake_script(&temp, name, body))],
        Arc::new(UnusedFakeHttpClient),
    )
    .unwrap();

    registry
        .complete("claude", &request("build a plan"))
        .unwrap_err()
        .to_string()
}

fn arg_after<'a>(args: &'a [&str], flag: &str) -> &'a str {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1])
        .unwrap_or_default()
}
