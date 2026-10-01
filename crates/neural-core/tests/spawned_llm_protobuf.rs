use lumvise_neural_core::llm_providers::contract::LlmProvider;
use lumvise_neural_core::llm_providers::local::LocalLlmProvider;
use lumvise_neural_core::llm_providers::{LlmMessage, LlmRequest, LlmStreamEvent};
use lumvise_neural_core::process::{
    SpawnedEnvelope, SpawnedOperation, SpawnedWorker, StreamControl,
};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, SpawnConfig};
use std::time::Duration;
use tempfile::TempDir;

const FIXTURE: &str = env!("CARGO_BIN_EXE_spawned-engine-fixture");

#[test]
fn local_completion_uses_real_protobuf_child() {
    let provider = provider(vec![]);
    let response = provider.complete(&request(false)).unwrap();

    assert_eq!(response.content, "fixture response");
    assert_eq!(response.model, "request-model");
}

#[test]
fn local_stream_delivers_data_before_child_exit() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("exited");
    let provider = provider(stream_marker_args(&marker));
    let mut events = Vec::new();
    provider
        .stream_with_events(&request(true), StreamControl::unbounded(), &mut |event| {
            if events.is_empty() {
                assert!(!marker.exists());
            }
            events.push(event);
            Ok(())
        })
        .unwrap();

    assert!(marker.exists());
    assert!(matches!(events[0], LlmStreamEvent::ContentDelta { .. }));
    assert_eq!(events[1], LlmStreamEvent::Complete);
}

#[test]
fn local_stream_cancellation_terminates_child() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("exited");
    let provider = provider(stream_marker_args(&marker));
    let events = provider
        .stream(&request(true), StreamControl::cancel_after(1))
        .unwrap();

    assert!(!marker.exists());
    assert!(matches!(events[0], LlmStreamEvent::ContentDelta { .. }));
    assert_eq!(events[1], LlmStreamEvent::Cancelled);
}

#[test]
fn controlled_deadline_reaps_silent_child_and_next_invocation_succeeds() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("silent-once");
    let config = spawn(vec![
        "--mode".into(),
        "silent-once".into(),
        "--state".into(),
        state.display().to_string(),
    ]);
    let worker =
        SpawnedWorker::with_controlled_deadline(config, Duration::from_millis(30)).unwrap();
    let first = worker
        .run_protobuf(SpawnedEnvelope::request(
            SpawnedOperation::TextVector,
            "first",
        ))
        .unwrap_err()
        .to_string();
    let second = worker
        .run_protobuf(SpawnedEnvelope::request(
            SpawnedOperation::TextVector,
            "second",
        ))
        .unwrap();

    assert!(first.contains("30ms"));
    assert_eq!(second.vector, vec![0.4, 0.5]);
}

fn provider(args: Vec<String>) -> LocalLlmProvider {
    LocalLlmProvider::new(LlmProviderConfig {
        provider_id: "local-fixture".into(),
        kind: LlmProviderKind::Local,
        model: "configured-model".into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(spawn(args)),
    })
    .unwrap()
}

fn stream_marker_args(marker: &std::path::Path) -> Vec<String> {
    vec![
        "--exit-marker".into(),
        marker.display().to_string(),
        "--stream-delay-ms".into(),
        "100".into(),
    ]
}

fn spawn(args: Vec<String>) -> SpawnConfig {
    SpawnConfig {
        command: FIXTURE.into(),
        args,
        timeout_ms: 1,
    }
}

fn request(stream: bool) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: "hello fixture".into(),
        }],
        stream,
        provider_id: None,
        model: Some("request-model".into()),
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: vec![],
        modality_inputs: vec![],
    }
}
