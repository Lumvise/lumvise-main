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

const FAKE_CODEX_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone)]
struct UnusedCodexHttpClient;

impl LlmHttpClient for UnusedCodexHttpClient {
    fn post_json(&self, _request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("codex provider should use configured CLI instead of HTTP")
    }

    fn stream_text(&self, _request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!("codex provider should use configured CLI instead of HTTP")
    }
}

#[test]
fn codex_provider_stateless_cli_writes_output_file_and_parses_response() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("stateless.log");
    let command = fake_script(
        &temp,
        "fake-codex-stateless",
        &format!(
            r#"printf '%s\n' "$*" >> '{}'
output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
if [ -z "$output" ]; then echo 'missing -o output path' >&2; exit 12; fi
printf '%s\n' '{{"items":[{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"stateless parsed"}}]}}]}}' > "$output""#,
            log_path.display()
        ),
    );
    let registry = codex_registry(command);

    let response = registry.complete("codex", &codex_request(false)).unwrap();
    let observed_args = fs::read_to_string(log_path).unwrap();

    assert_eq!(response.provider_id, "codex");
    assert_eq!(response.model, "gpt-test");
    assert_eq!(response.content, "stateless parsed");
    assert!(observed_args.contains("exec"));
    assert!(observed_args.contains("-o"));
}

#[test]
fn codex_provider_stateless_cli_resumes_and_returns_session_id() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("stateless-resume.log");
    let command = fake_script(
        &temp,
        "fake-codex-stateless-resume",
        &format!(
            r#"printf '%s\n' "$*" >> '{}'
output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
if [ -z "$output" ]; then echo 'missing -o output path' >&2; exit 12; fi
printf '%s\n' '{{"type":"session_meta","id":"codex-session-next"}}' >&2
printf '%s\n' '{{"response":"resumed parsed"}}' > "$output""#,
            log_path.display()
        ),
    );
    let registry = codex_registry(command);
    let mut request = codex_request(false);
    request.provider_session_id = Some("codex-session-existing".to_string());

    let response = registry.complete("codex", &request).unwrap();
    let observed_args = fs::read_to_string(log_path).unwrap();

    assert_eq!(response.content, "resumed parsed");
    assert_eq!(
        response.metadata["provider_session_id"],
        serde_json::json!("codex-session-next")
    );
    assert!(observed_args.contains("resume codex-session-existing"));
}

#[test]
fn codex_provider_stream_reports_final_text_from_cli_file_response() {
    let temp = TempDir::new().unwrap();
    let command = fake_script(
        &temp,
        "fake-codex-stream",
        r#"output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
if [ -z "$output" ]; then echo 'missing -o output path' >&2; exit 12; fi
printf '%s\n' '{"response":"streamed delta"}' > "$output""#,
    );
    let registry = codex_registry(command);

    let events = registry
        .stream("codex", &codex_request(true), StreamControl::unbounded())
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::FinalText {
                text: "streamed delta".to_string()
            },
            LlmStreamEvent::Complete
        ]
    );
}

#[test]
fn codex_provider_stream_emits_jsonl_message_before_process_completion() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("stream-order.log");
    let ack_path = temp.path().join("stream-callback-seen");
    let command = fake_script(
        &temp,
        "fake-codex-jsonl-stream",
        &format!(
            r#"if [ "$1" = 'debug' ] && [ "$2" = 'models' ]; then
  printf '%s\n' '{{"models":[{{"slug":"gpt-test"}}]}}'
  exit 0
fi
printf '%s\n' '{{"type":"item.completed","item":{{"type":"agent_message","text":"early jsonl"}}}}'
while [ ! -f '{}' ]; do sleep 0.01; done
printf '%s\n' complete >> '{}'
output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
printf '%s\n' '{{"response":"late output"}}' > "$output""#,
            ack_path.display(),
            log_path.display()
        ),
    );
    let registry = codex_registry(command);
    let mut events = Vec::new();

    registry
        .stream_with_events(
            "codex",
            &codex_request(true),
            StreamControl::unbounded(),
            &mut |event| {
                if matches!(event, LlmStreamEvent::ContentDelta { .. }) {
                    let log = fs::read_to_string(&log_path).unwrap_or_default();
                    assert!(!log.contains("complete"));
                    fs::write(&ack_path, "seen").unwrap();
                }
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(delta_texts(&events), vec!["early jsonl"]);
    assert!(matches!(events.last(), Some(LlmStreamEvent::Complete)));
}

#[test]
fn codex_provider_stream_emits_agent_delta_before_completed_snapshot() {
    let temp = TempDir::new().unwrap();
    let ack_path = temp.path().join("delta-seen");
    let command = fake_script(
        &temp,
        "fake-codex-agent-delta-stream",
        &format!(
            r#"if [ "$1" = 'debug' ] && [ "$2" = 'models' ]; then
  printf '%s\n' '{{"models":[{{"slug":"gpt-test"}}]}}'
  exit 0
fi
output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
printf '%s\n' '{{"type":"agent_message.delta","delta":"early "}}'
printf '%s\n' '{{"type":"item.delta","delta":{{"text":"delta"}}}}'
while [ ! -f '{}' ]; do sleep 0.01; done
printf '%s\n' '{{"type":"item.completed","item":{{"type":"agent_message","text":"early delta"}}}}'
printf '%s\n' '{{"type":"turn.completed"}}'
printf '%s\n' '{{"response":"late output"}}' > "$output""#,
            ack_path.display()
        ),
    );
    let registry = codex_registry(command);
    let mut events = Vec::new();

    registry
        .stream_with_events(
            "codex",
            &codex_request(true),
            StreamControl::unbounded(),
            &mut |event| {
                if delta_texts(std::slice::from_ref(&event)) == ["delta"] {
                    fs::write(&ack_path, "seen").unwrap();
                }
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(delta_texts(&events), vec!["early ", "delta"]);
    assert!(matches!(events.last(), Some(LlmStreamEvent::Complete)));
}

#[test]
fn codex_provider_reuses_hot_session_when_command_basename_is_codex() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("hot.log");
    let command = fake_script(
        &temp,
        "codex",
        &format!(
            r#"printf '%s\n' "$*" >> '{}'
output=''
previous=''
resume='no'
session_seen='no'
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  if [ "$arg" = 'resume' ]; then resume='yes'; fi
  if [ "$arg" = 'session-1' ]; then session_seen='yes'; fi
  previous="$arg"
done
if [ -z "$output" ]; then echo 'missing -o output path' >&2; exit 12; fi
if [ "$resume" = 'yes' ]; then
  if [ "$session_seen" != 'yes' ]; then echo 'missing session reuse' >&2; exit 13; fi
  printf '%s\n' '{{"response":"second hot"}}' > "$output"
else
  printf '%s\n' '{{"type":"session_meta","payload":{{"id":"session-1"}}}}'
  printf '%s\n' '{{"response":"first hot"}}' > "$output"
fi"#,
            log_path.display()
        ),
    );
    let registry = codex_registry(command);

    let first = registry.complete("codex", &codex_request(false)).unwrap();
    let second = registry.complete("codex", &codex_request(false)).unwrap();
    let invocations = fs::read_to_string(log_path).unwrap();

    assert_eq!(first.content, "first hot");
    assert_eq!(second.content, "second hot");
    let codex_turns = invocations
        .lines()
        .filter(|line| line.contains("exec"))
        .collect::<Vec<_>>();
    assert!(codex_turns.first().unwrap().contains("exec"));
    assert!(codex_turns.get(1).unwrap().contains("resume"));
    assert!(codex_turns.get(1).unwrap().contains("session-1"));
}

#[test]
fn codex_provider_default_model_omits_model_argument() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("provider-default.log");
    let command = fake_script(
        &temp,
        "codex",
        &format!(
            r#"printf '%s\n' "$*" >> '{}'
output=''
previous=''
for arg in "$@"; do
  if [ "$arg" = '--model' ]; then echo 'unexpected model argument' >&2; exit 14; fi
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
if [ -z "$output" ]; then echo 'missing -o output path' >&2; exit 12; fi
printf '%s\n' '{{"response":"provider default"}}' > "$output""#,
            log_path.display()
        ),
    );
    let registry = codex_registry_with_model(command, "provider-default");

    let response = registry.complete("codex", &codex_request(false)).unwrap();
    let observed_args = fs::read_to_string(log_path).unwrap();

    assert_eq!(response.content, "provider default");
    assert!(observed_args.contains("exec"));
    assert!(!observed_args.contains("--model"));
}
#[test]
fn codex_provider_passes_request_mcp_server_to_persistent_process() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("mcp-server.log");
    let command = fake_python_script(
        &temp,
        "codex",
        &format!(
            r#"
import json, os, sys, tomllib
with open(os.path.join(os.environ['CODEX_HOME'], 'config.toml'), 'rb') as config:
    servers = tomllib.load(config)['mcp_servers']
with open({log_path:?}, 'w') as log:
    json.dump(servers, log)
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    result = {{}} if method == 'initialize' else {{'thread': {{'id':'configured'}}}} if method == 'thread/start' else {{'turn': {{'id':'turn-1'}}}}
    print(json.dumps({{'id':request['id'], 'result':result}}), flush=True)
    if method == 'turn/start':
        print(json.dumps({{'method':'turn/completed', 'params':{{'threadId':'configured', 'turn':{{'id':'turn-1','status':'completed','items':[{{'type':'agentMessage','text':'mcp configured'}}]}}}}}}), flush=True)

"#,
            log_path = log_path.to_string_lossy()
        ),
    );
    let registry = codex_registry_with_model(command, "provider-default");

    let response = registry
        .complete("codex", &codex_request_with_mcp(false))
        .unwrap();
    let servers: Value = serde_json::from_str(&fs::read_to_string(log_path).unwrap()).unwrap();
    assert_eq!(response.content, "mcp configured");
    assert_eq!(servers.as_object().unwrap().len(), 1);
    assert_eq!(servers["lumvise-assistant"]["enabled"], true);
    assert_eq!(
        servers["lumvise-assistant"]["default_tools_approval_mode"],
        "auto"
    );
    assert_eq!(
        servers["lumvise-assistant"]["url"],
        "http://127.0.0.1:4180/mcp/messages"
    );
}

#[test]
fn codex_provider_uses_catalog_selected_model_without_runtime_inventory() {
    let temp = TempDir::new().unwrap();
    let log_path = temp.path().join("codex-args.log");
    let command = fake_script(
        &temp,
        "codex-catalog-model",
        &format!(
            r#"printf '%s\n' "$*" >> '{}'
output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
printf '%s\n' '{{"response":"catalog authority"}}' > "$output""#,
            log_path.display()
        ),
    );
    let registry = codex_registry_with_model(command, "catalog-client-model");
    let response = registry.complete("codex", &codex_request(false)).unwrap();
    let observed_args = fs::read_to_string(log_path).unwrap();

    assert_eq!(response.content, "catalog authority");
    assert!(observed_args.contains("--model catalog-client-model"));
    assert!(!observed_args.contains("debug models"));
}

#[test]
fn codex_provider_stream_cancellation_does_not_emit_complete() {
    let temp = TempDir::new().unwrap();
    let command = fake_script(
        &temp,
        "fake-codex-cancel",
        r#"output=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '-o' ]; then output="$arg"; fi
  previous="$arg"
done
printf '%s\n' '{"response":"cancel me"}' > "$output""#,
    );
    let registry = codex_registry(command);

    let events = registry
        .stream(
            "codex",
            &codex_request(true),
            StreamControl::cancel_after(1),
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::FinalText {
                text: "cancel me".to_string()
            },
            LlmStreamEvent::Cancelled
        ]
    );
}

#[test]
fn codex_provider_hot_stream_reports_command_failure() {
    let temp = TempDir::new().unwrap();
    let command = fake_script(
        &temp,
        "codex",
        r#"echo 'codex exploded' >&2
exit 17"#,
    );
    let registry = codex_registry(command);

    let error = registry
        .stream("codex", &codex_request(true), StreamControl::unbounded())
        .unwrap_err()
        .to_string();

    assert!(error.contains("codex"));
    assert!(error.contains("failed"));
    assert!(error.contains("codex exploded"));
}

#[test]
fn codex_provider_rejects_live_modality_inputs() {
    let temp = TempDir::new().unwrap();
    let command = fake_script(
        &temp,
        "fake-codex-modality",
        "echo should not run >&2\nexit 12",
    );
    let registry = codex_registry(command);

    let audio_error = codex_modality_error(&registry, LlmModalityInputKind::LiveAudioChunk);
    let frame_error = codex_modality_error(&registry, LlmModalityInputKind::ScreenFrame);

    assert!(audio_error.contains("live_audio_input is unsupported"));
    assert!(frame_error.contains("screen_frame_broadcast_input is unsupported"));
}

fn codex_registry(command: String) -> LlmProviderRegistry {
    codex_registry_with_model(command, "gpt-test")
}

fn codex_registry_with_model(command: String, model: &str) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(
        vec![LlmProviderConfig {
            provider_id: "codex".to_string(),
            kind: LlmProviderKind::Codex,
            model: model.to_string(),
            endpoint: None,
            credential: None,
            completion_concurrency: None,
            spawn: Some(SpawnConfig {
                command,
                args: vec![],
                timeout_ms: FAKE_CODEX_TIMEOUT_MS,
            }),
        }],
        Arc::new(UnusedCodexHttpClient),
    )
    .unwrap()
}

fn codex_modality_error(registry: &LlmProviderRegistry, kind: LlmModalityInputKind) -> String {
    let mut request = codex_request(false);
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "modality-1".to_string(),
        kind,
        media_type: "application/octet-stream".to_string(),
        bytes: vec![1],
        metadata: serde_json::json!({}),
    }];

    registry
        .complete("codex", &request)
        .unwrap_err()
        .to_string()
}

fn codex_request(stream: bool) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello codex".to_string(),
        }],
        stream,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

fn codex_request_with_mcp(stream: bool) -> LlmRequest {
    LlmRequest {
        mcp_servers: vec![LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: "http://127.0.0.1:4180/mcp/sse".to_string(),
        }],
        ..codex_request(stream)
    }
}

fn delta_texts(events: &[LlmStreamEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            LlmStreamEvent::ContentDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn fake_script(temp: &TempDir, name: &str, body: &str) -> String {
    let path = temp.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path.to_string_lossy().to_string()
}

fn fake_python_script(temp: &TempDir, name: &str, body: &str) -> String {
    let path = temp.path().join(name);
    fs::write(&path, format!("#!/usr/bin/env python3\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path.to_string_lossy().to_string()
}
