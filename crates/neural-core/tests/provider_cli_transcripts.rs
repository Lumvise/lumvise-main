use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{LlmMessage, LlmRequest, LlmStreamEvent};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FIXTURE_EXECUTABLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/tests/provider-cli-transcript-fixture"
);

struct NoHttp;

impl LlmHttpClient for NoHttp {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("CLI fixture unexpectedly called HTTP: {}", request.endpoint)
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!(
            "CLI fixture unexpectedly streamed HTTP: {}",
            request.endpoint
        )
    }
}

#[test]
fn codex_current_transcript_preserves_thread_and_stream_contract() {
    let registry = registry(codex_config("success"));
    let response = registry.complete("codex", &request(false)).unwrap();
    let events = registry
        .stream("codex", &request(true), StreamControl::unbounded())
        .unwrap();

    assert_eq!(response.content, "fixture codex response");
    assert_eq!(
        response.metadata["provider_session_id"],
        "019c-codex-fixture-thread"
    );
    assert!(matches!(events[0], LlmStreamEvent::Session { .. }));
    assert!(matches!(events[1], LlmStreamEvent::ContentDelta { .. }));
    assert_eq!(events[2], LlmStreamEvent::Complete);
}

#[test]
fn codex_current_transcript_reports_structured_malformed_and_process_failures() {
    let structured = stream_error(codex_config("structured-error"), "codex");
    let malformed = complete_error(codex_config("malformed"), "codex");
    let process = complete_error(codex_config("nonzero"), "codex");

    assert!(structured.contains("captured Codex structured failure"));
    assert!(malformed.contains("non-empty Codex response"));
    assert!(process.contains("fixture process failure"));
}

#[test]
fn gemini_current_transcript_preserves_text_and_completion_contract() {
    let registry = registry(gemini_config("success"));
    let response = registry.complete("gemini", &request(false)).unwrap();
    let events = registry
        .stream("gemini", &request(true), StreamControl::unbounded())
        .unwrap();

    assert_eq!(response.content, "fixture gemini response");
    assert_eq!(delta_text(&events), "fixture gemini response");
    assert_eq!(events.last(), Some(&LlmStreamEvent::Complete));
}

#[test]
fn gemini_current_transcript_reports_structured_malformed_and_process_failures() {
    let structured = stream_error(gemini_config("structured-error"), "gemini");
    let malformed = complete_error(gemini_config("malformed"), "gemini");
    let process = complete_error(gemini_config("nonzero"), "gemini");

    assert!(structured.contains("captured Gemini structured failure"));
    assert!(malformed.contains("Gemini JSON stdout"));
    assert!(process.contains("fixture process failure"));
}

fn registry(config: LlmProviderConfig) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(vec![config], Arc::new(NoHttp)).unwrap()
}

fn codex_config(mode: &str) -> LlmProviderConfig {
    provider_config("codex", LlmProviderKind::Codex, "provider-default", mode)
}

fn gemini_config(mode: &str) -> LlmProviderConfig {
    provider_config("gemini", LlmProviderKind::Gemini, "flash", mode)
}

fn provider_config(
    provider: &str,
    kind: LlmProviderKind,
    model: &str,
    mode: &str,
) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: provider.into(),
        kind,
        model: model.into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(transcript_spawn(provider, mode)),
    }
}

fn transcript_spawn(provider: &str, mode: &str) -> SpawnConfig {
    SpawnConfig {
        command: FIXTURE_EXECUTABLE.into(),
        args: vec![
            "--fixture-provider".into(),
            provider.into(),
            "--fixture-mode".into(),
            mode.into(),
            "--fixture-root".into(),
            fixture_root(provider).display().to_string(),
        ],
        timeout_ms: 5_000,
    }
}

fn fixture_root(provider: &str) -> PathBuf {
    let version = if provider == "codex" {
        "codex-0.144.4"
    } else {
        "gemini-0.46.0"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/provider-cli")
        .join(version)
}

fn request(stream: bool) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: "reply from transcript".into(),
        }],
        stream,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: vec![],
        modality_inputs: vec![],
    }
}

fn complete_error(config: LlmProviderConfig, provider: &str) -> String {
    registry(config)
        .complete(provider, &request(false))
        .unwrap_err()
        .to_string()
}

fn stream_error(config: LlmProviderConfig, provider: &str) -> String {
    registry(config)
        .stream(provider, &request(true), StreamControl::unbounded())
        .unwrap_err()
        .to_string()
}

fn delta_text(events: &[LlmStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            LlmStreamEvent::ContentDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
