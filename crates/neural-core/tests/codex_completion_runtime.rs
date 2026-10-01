//! Exercises the same controlled completion interface used by Assistant background jobs.
use lumvise_neural_core::llm_providers::codex::CodexProvider;
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmProvider};
use lumvise_neural_core::llm_providers::{
    LlmExecutionControl, LlmExecutorRegistry, LlmMcpServerConfig, LlmMessage, LlmRequest,
};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;

struct NoCodexHttp;
impl LlmHttpClient for NoCodexHttp {
    fn post_json(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("expected CLI")
    }
    fn stream_text(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!("expected CLI")
    }
}

struct FakePersistentCodex {
    root: TempDir,
}
impl FakePersistentCodex {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("codex");
        fs::write(
            &script,
            concat!(
                "#!/usr/bin/env python3\n",
                include_str!("support/codex_app_server.py")
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }
    fn provider(&self) -> CodexProvider {
        CodexProvider::new(self.config(), Arc::new(NoCodexHttp)).unwrap()
    }
    fn config(&self) -> LlmProviderConfig {
        LlmProviderConfig {
            provider_id: "codex".into(),
            kind: LlmProviderKind::Codex,
            model: "test-model".into(),
            endpoint: None,
            credential: None,
            completion_concurrency: None,
            spawn: Some(SpawnConfig {
                command: self.root.path().join("codex").to_string_lossy().into(),
                args: vec![],
                timeout_ms: 5_000,
            }),
        }
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.root.path().join("calls")).unwrap_or_default()
    }
}

fn request(prompt: &str) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: prompt.into(),
        }],
        stream: false,
        provider_id: None,
        model: Some("test-model".into()),
        conversation_id: Some("assistant-session".into()),
        provider_session_id: None,
        mcp_servers: vec![LlmMcpServerConfig {
            name: "assistant".into(),
            url: "http://127.0.0.1:1/assistant-session/mcp".into(),
        }],
        modality_inputs: vec![],
    }
}

#[test]
fn assistant_completion_reuses_process_and_thread_through_executor() {
    let fake = FakePersistentCodex::new();
    let providers =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(fake.provider())]).unwrap();
    let executor = LlmExecutorRegistry::new(Arc::new(Mutex::new(providers)));
    for prompt in ["hello", "tool-only"] {
        let control =
            LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_secs(5)));
        let answer = executor
            .complete("codex", request(prompt), control)
            .unwrap();
        assert_eq!(
            answer.content,
            if prompt == "hello" {
                "final answer"
            } else {
                ""
            }
        );
        assert_eq!(answer.metadata["provider_session_id"], "thread-1");
    }
    assert_eq!(fake.calls().matches("spawn ").count(), 1);
    assert!(
        fake.calls()
            .contains("thread/start new\nturn/start thread-1\nturn/start thread-1")
    );
}

#[test]
fn independent_codex_conversation_completes_while_another_is_stalled() {
    let fake = FakePersistentCodex::new();
    let providers =
        LlmProviderRegistry::from_configs(vec![fake.config()], Arc::new(NoCodexHttp)).unwrap();
    let executor = LlmExecutorRegistry::new(Arc::new(Mutex::new(providers)));
    let stalled_control = InvocationControl::with_deadline(Duration::from_secs(5));
    let first_executor = Arc::clone(&executor);
    let first_control = stalled_control.clone();
    let stalled = std::thread::spawn(move || {
        first_executor.complete(
            "codex",
            request("stall"),
            LlmExecutionControl::new(first_control),
        )
    });
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while !fake.calls().contains("waiting") && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        fake.calls().contains("waiting"),
        "first turn must reach app-server"
    );
    let mut independent = request("hello");
    independent.conversation_id = Some("independent-session".into());
    independent.mcp_servers[0].url = "http://127.0.0.1:1/independent-session/mcp".into();
    let answer = executor.complete(
        "codex",
        independent.clone(),
        LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_millis(600))),
    );
    stalled_control.cancel();
    assert_eq!(
        stalled.join().unwrap().unwrap_err().code,
        lumvise_neural_core::llm_providers::LlmFailureCode::Cancelled
    );
    assert_eq!(
        answer
            .expect("independent session must not wait for stalled session")
            .content,
        "final answer"
    );
    assert_eq!(
        executor
            .complete(
                "codex",
                independent,
                LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_secs(5)))
            )
            .unwrap()
            .content,
        "final answer"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 2);
}

#[test]
fn same_codex_conversation_waits_without_starting_an_overlapping_turn() {
    let fake = FakePersistentCodex::new();
    let provider = Arc::new(fake.provider());
    let stalled_control = InvocationControl::with_deadline(Duration::from_secs(5));
    let first_provider = Arc::clone(&provider);
    let first_control = stalled_control.clone();
    let stalled = std::thread::spawn(move || {
        first_provider.complete_controlled(&request("stall"), &first_control)
    });
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while !fake.calls().contains("waiting") && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fake.calls().contains("waiting"));
    let second = provider.complete_controlled(
        &request("hello"),
        &InvocationControl::with_deadline(Duration::from_millis(100)),
    );
    stalled_control.cancel();
    let _ = stalled.join().unwrap();
    assert!(matches!(
        second,
        Err(lumvise_neural_core::NeuralError::ProcessTimeout { .. })
    ));
    assert_eq!(
        fake.calls().matches("turn/start ").count(),
        1,
        "a waiting turn must not enter the busy session"
    );
    assert_eq!(
        provider.complete(&request("hello")).unwrap().content,
        "final answer"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 2);
}

#[test]
fn completion_deadline_kills_stalled_process_and_next_turn_recovers() {
    let fake = FakePersistentCodex::new();
    let provider = fake.provider();
    provider.complete(&request("warmup")).unwrap();
    let control = InvocationControl::with_deadline(Duration::from_millis(600));
    let failure = provider
        .complete_controlled(&request("stall"), &control)
        .unwrap_err();
    assert!(matches!(
        failure,
        lumvise_neural_core::NeuralError::ProcessTimeout { .. }
    ));
    assert_eq!(
        provider.complete(&request("hello")).unwrap().content,
        "final answer"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 2);
}

#[test]
fn completion_deadline_covers_app_server_initialization() {
    let fake = FakePersistentCodex::new();
    fs::write(fake.root.path().join("stall_init"), "").unwrap();
    let provider = fake.provider();
    let started = std::time::Instant::now();
    let control = ExpireDuringInitialization(&fake);
    let failure = provider
        .complete_controlled(&request("hello"), &control)
        .unwrap_err();
    assert!(matches!(
        failure,
        lumvise_neural_core::NeuralError::ProcessTimeout { .. }
    ));
    assert_eq!(fake.calls().matches("spawn ").count(), 1);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn completion_recovers_missing_thread_without_restarting_process() {
    let fake = FakePersistentCodex::new();
    let provider = fake.provider();
    let mut turn = request("hello");
    turn.provider_session_id = Some("stale".into());
    assert_eq!(
        provider.complete(&turn).unwrap().metadata["provider_session_id"],
        "thread-1"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 1);
    assert!(
        fake.calls()
            .contains("thread/resume stale\nthread/start new")
    );
}

struct CancelAfterAcceptedTurn<'a>(&'a FakePersistentCodex);
impl lumvise_neural_core::llm_providers::contract::ProviderCallControl
    for CancelAfterAcceptedTurn<'_>
{
    fn remaining(&self) -> Duration {
        Duration::from_secs(5)
    }
    fn is_cancelled(&self) -> bool {
        self.0.calls().contains("waiting")
    }
}

#[test]
fn completion_cancellation_terminates_process_and_next_turn_recovers() {
    let fake = FakePersistentCodex::new();
    let provider = fake.provider();
    provider.complete(&request("warmup")).unwrap();
    let failure = provider
        .complete_controlled(&request("stall"), &CancelAfterAcceptedTurn(&fake))
        .unwrap_err();
    assert!(matches!(
        failure,
        lumvise_neural_core::NeuralError::ProcessCancelled { .. }
    ));
    assert_eq!(
        provider.complete(&request("hello")).unwrap().content,
        "final answer"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 2);
}

#[test]
fn completion_process_crash_is_not_reused() {
    let fake = FakePersistentCodex::new();
    let provider = fake.provider();
    assert!(provider.complete(&request("crash")).is_err());
    assert_eq!(
        provider.complete(&request("hello")).unwrap().content,
        "final answer"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 2);
}

#[test]
fn streaming_and_completion_share_thread_and_ignore_unrelated_events() {
    let fake = FakePersistentCodex::new();
    let provider = fake.provider();
    let events = provider
        .stream(
            &request("hello"),
            lumvise_neural_core::process::StreamControl::unbounded(),
        )
        .unwrap();
    let deltas = events
        .iter()
        .filter_map(|event| match event {
            lumvise_neural_core::llm_providers::LlmStreamEvent::ContentDelta { text } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<String>();
    assert_eq!(deltas, "final answer");
    assert!(events.contains(&lumvise_neural_core::llm_providers::LlmStreamEvent::Complete));
    assert_eq!(
        provider.complete(&request("next")).unwrap().content,
        "final answer"
    );
    assert_eq!(fake.calls().matches("spawn ").count(), 1);
    assert_eq!(fake.calls().matches("thread/start new").count(), 1);
}

#[test]
fn app_server_errors_do_not_become_successful_empty_answers() {
    for prompt in ["rpc-error", "fail-turn"] {
        let fake = FakePersistentCodex::new();
        let provider = fake.provider();
        assert!(provider.complete(&request(prompt)).is_err());
        assert_eq!(
            provider.complete(&request("recover")).unwrap().content,
            "final answer"
        );
        assert_eq!(fake.calls().matches("spawn ").count(), 2);
    }
}

#[test]
fn initialization_rejection_stops_before_creating_a_thread() {
    let fake = FakePersistentCodex::new();
    fs::write(fake.root.path().join("reject_init"), "").unwrap();
    assert!(fake.provider().complete(&request("hello")).is_err());
    assert!(!fake.calls().contains("thread/start"));
}

#[test]
fn thread_resume_preserves_identity_and_does_not_hide_permission_errors() {
    for thread in ["thread-1", "denied"] {
        let fake = FakePersistentCodex::new();
        let mut turn = request("hello");
        turn.provider_session_id = Some(thread.into());
        let result = fake.provider().complete(&turn);
        assert_eq!(result.is_ok(), thread == "thread-1");
        assert!(fake.calls().contains(&format!("thread/resume {thread}")));
        assert!(!fake.calls().contains("thread/start"));
    }
}

#[test]
fn app_server_preserves_notifications_before_turn_acknowledgement() {
    let fake = FakePersistentCodex::new();
    assert_eq!(
        fake.provider()
            .complete(&request("early-events"))
            .unwrap()
            .content,
        "final answer"
    );
}

struct ExpireDuringInitialization<'a>(&'a FakePersistentCodex);
impl lumvise_neural_core::llm_providers::contract::ProviderCallControl
    for ExpireDuringInitialization<'_>
{
    fn is_cancelled(&self) -> bool {
        false
    }
    fn remaining(&self) -> Duration {
        Duration::from_secs(5)
    }
    fn is_expired(&self) -> bool {
        self.0.calls().contains("initialize")
    }
}
