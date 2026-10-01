use super::*;
use crate::workspace_activity::{ActivityKind, ActivityStatus};
use crate::{AppCore, AppCoreError};
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_neural_core::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use lumvise_neural_core::llm_providers::{
    LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities, LlmResponse,
};
use lumvise_neural_core::process::StreamControl;
use serde_json::{Value, json};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NEURAL_LLM: &str = "neural.llm";

struct GatedStreamingProvider {
    entered: SyncSender<()>,
    release: Mutex<Receiver<()>>,
}

impl LlmProvider for GatedStreamingProvider {
    fn provider_id(&self) -> &str {
        "activity-test"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: self.provider_id().into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        Ok(LlmResponse {
            provider_id: self.provider_id().into(),
            model: request.model.clone().unwrap_or_default(),
            content: "assistant completed".into(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        let _ = self.entered.send(());
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| lumvise_neural_core::NeuralError::ProviderFailed {
                provider_id: self.provider_id().into(),
                message: format!("stream release gate failed: {error}"),
            })?;
        on_event(
            lumvise_neural_core::llm_providers::LlmStreamEvent::ContentDelta {
                text: "stream completed".into(),
            },
        )?;
        Ok(())
    }
}

fn app_with_gated_provider() -> (Arc<AppCore>, Receiver<()>, SyncSender<()>) {
    let (entered, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(GatedStreamingProvider {
            entered,
            release: Mutex::new(release_rx),
        })])
        .expect("gated streaming provider registry");
    (
        Arc::new(AppCore::in_memory_with_llm_registry(registry).expect("in-memory app")),
        entered_rx,
        release_tx,
    )
}

fn stream_request() -> LlmRequest {
    LlmRequest {
        messages: vec![LlmMessage {
            role: "user".into(),
            content: "hold this stream".into(),
        }],
        stream: true,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
        options: Default::default(),
    }
}

fn assistant_request() -> Value {
    json!({
        "provider_id": "activity-test",
        "model": null,
        "conversation_id": null,
        "llm_session_id": null,
        "messages": [{"role": "user", "content": "finish independently"}],
        "mcp_servers": []
    })
}

#[test]
fn held_ordinary_stream_does_not_block_assistant_and_reports_running_activity() {
    let (app, entered, release) = app_with_gated_provider();
    let stream_app = Arc::clone(&app);
    let stream = std::thread::spawn(move || {
        stream_app.llms().stream(
            "activity-test",
            &stream_request(),
            StreamControl::unbounded(),
        )
    });
    let stream_entered = entered.recv_timeout(Duration::from_secs(2)).is_ok();

    let assistant_app = Arc::clone(&app);
    let (assistant_tx, assistant_rx) = mpsc::channel();
    let assistant = std::thread::spawn(move || {
        let result = assistant_app.plugin_host_services.invoke(
            "builtin.assistant",
            NEURAL_LLM,
            assistant_request(),
        );
        let _ = assistant_tx.send(result);
    });
    let assistant_while_stream_held = assistant_rx.recv_timeout(Duration::from_secs(1)).ok();
    let held_snapshot =
        app.plugin_host_services
            .activity()
            .wait_for_changes("/repo", None, Duration::ZERO);
    let assistant_finished_while_held = assistant_while_stream_held.is_some();

    let _ = release.send(());
    let stream_result = stream.join().expect("stream thread");
    assistant.join().expect("Assistant thread");
    let assistant_result = assistant_while_stream_held
        .or_else(|| assistant_rx.recv_timeout(Duration::from_secs(2)).ok());
    let completed_entries = app
        .plugin_host_services
        .activity()
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .expect("completed activity snapshot");

    assert!(stream_entered, "ordinary provider stream did not enter");
    assert!(
        assistant_result.is_some(),
        "Assistant invocation did not complete after stream release"
    );
    assert!(
        assistant_finished_while_held,
        "Assistant invocation waited for the held ordinary stream"
    );
    assert_eq!(
        assistant_result
            .expect("Assistant result")
            .expect("Assistant invocation")["response"]["content"],
        "assistant completed"
    );
    assert_eq!(
        stream_result.expect("ordinary stream completes after release")[0],
        lumvise_neural_core::llm_providers::LlmStreamEvent::ContentDelta {
            text: "stream completed".into()
        }
    );
    let entries = held_snapshot.entries.expect("activity changed");
    assert!(entries.iter().any(|entry| {
        entry.kind == ActivityKind::Llm
            && entry.title == "LLM request · activity-test"
            && entry.status == ActivityStatus::Running
    }));
    assert!(entries.iter().any(|entry| {
        entry.kind == ActivityKind::Assistant && entry.status == ActivityStatus::Succeeded
    }));
    assert!(completed_entries.iter().any(|entry| {
        entry.kind == ActivityKind::Llm
            && entry.title == "LLM request · activity-test"
            && entry.status == ActivityStatus::Succeeded
    }));
}

#[test]
fn stream_callback_error_is_preserved_and_activity_fails() {
    let (app, entered, release) = app_with_gated_provider();
    let stream_app = Arc::clone(&app);
    let stream = std::thread::spawn(move || {
        stream_app.llms().stream_with_events(
            "activity-test",
            &stream_request(),
            StreamControl::unbounded(),
            &mut |_| {
                Err(AppCoreError::invalid_value(
                    "callback sentinel",
                    "successful callback",
                ))
            },
        )
    });
    let stream_entered = entered.recv_timeout(Duration::from_secs(2)).is_ok();
    let _ = release.send(());
    let result = stream.join().expect("stream thread");
    let entries = app
        .plugin_host_services
        .activity()
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .expect("activity changed");

    assert!(stream_entered, "provider stream did not enter");
    assert!(matches!(
        result,
        Err(AppCoreError::InvalidValue { value, expected })
            if value == "callback sentinel" && expected == "successful callback"
    ));
    assert!(entries.iter().any(|entry| {
        entry.kind == ActivityKind::Llm
            && entry.title == "LLM request · activity-test"
            && entry.status == ActivityStatus::Failed
            && entry
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("app-core LLM stream callback failed"))
    }));
}
