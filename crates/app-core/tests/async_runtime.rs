use lumvise_app_core::{
    AppCore, AppCoreRuntime, CanvasElement, CanvasPatch, RuntimeSpawnMode, RuntimeWorkerState,
};
use lumvise_frontend_core::{VoiceAudioChunk, VoicePlaybackChunk, VoicePlaybackStatus, WorkArea};
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_neural_core::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use lumvise_neural_core::llm_providers::{
    LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities, LlmRequest, LlmResponse,
};
use lumvise_neural_core::process::StreamControl;
use serde_json::json;
use std::time::Duration;

struct SlowFakeProvider;

impl LlmProvider for SlowFakeProvider {
    fn provider_id(&self) -> &str {
        "local"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: "local".into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, _request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        std::thread::sleep(Duration::from_millis(350));
        Ok(LlmResponse {
            provider_id: "local".into(),
            model: "local-model".into(),
            content: "slow local response".into(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_workers_do_not_block_frontend_while_llm_runs() {
    let app = app_with_slow_llm();
    let runtime = AppCoreRuntime::start(app).unwrap();
    let llm_handle = runtime.llms();
    let llm_request = llm_request();

    let llm_task = tokio::spawn(async move { llm_handle.complete("local", llm_request).await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    let frontend_started = std::time::Instant::now();
    let canvas = runtime
        .frontend()
        .update_canvas(canvas_patch())
        .await
        .unwrap();
    let frontend_elapsed = frontend_started.elapsed();
    let llm_response = llm_task.await.unwrap().unwrap();

    assert_eq!(canvas.revision, 1);
    assert!(frontend_elapsed < Duration::from_millis(180));
    assert_eq!(llm_response.content, "slow local response");
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_health_reports_started_and_stopped_workers() {
    let runtime = AppCoreRuntime::start(AppCore::in_memory().unwrap()).unwrap();

    let started = runtime.health().await.unwrap();
    let stopped = runtime.shutdown().await.unwrap();

    assert_eq!(started.frontend, RuntimeWorkerState::Running);
    assert_eq!(started.modalities, RuntimeWorkerState::Running);
    assert_eq!(started.llms, RuntimeWorkerState::Running);
    assert_eq!(stopped.frontend, RuntimeWorkerState::Stopped);
    assert_eq!(stopped.modalities, RuntimeWorkerState::Stopped);
    assert_eq!(stopped.llms, RuntimeWorkerState::Stopped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_mode_skips_frontend_worker_but_keeps_core_workers_running() {
    let runtime = AppCoreRuntime::start_daemon(AppCore::in_memory().unwrap()).unwrap();

    let health = runtime.health().await.unwrap();
    let frontend_error = runtime
        .frontend()
        .update_canvas(canvas_patch())
        .await
        .unwrap_err();
    let provider_ids = runtime.llms().provider_ids().await.unwrap();

    assert_eq!(health.frontend, RuntimeWorkerState::Stopped);
    assert_eq!(health.modalities, RuntimeWorkerState::Running);
    assert_eq!(health.llms, RuntimeWorkerState::Running);
    assert!(frontend_error.to_string().contains("frontend"));
    assert!(provider_ids.is_empty());
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_mode_starts_frontend_worker() {
    let runtime =
        AppCoreRuntime::start_with_mode(AppCore::in_memory().unwrap(), RuntimeSpawnMode::App)
            .unwrap();

    let health = runtime.health().await.unwrap();
    let canvas = runtime
        .frontend()
        .update_canvas(canvas_patch())
        .await
        .unwrap();

    assert_eq!(health.frontend, RuntimeWorkerState::Running);
    assert_eq!(canvas.revision, 1);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_modalities_stream_speech_playback_and_desktop_media() {
    let runtime = AppCoreRuntime::start(AppCore::in_memory().unwrap()).unwrap();
    runtime
        .frontend()
        .spawn_app(WorkArea::new(0, 0, 1440, 900, 1.0))
        .await
        .unwrap();

    runtime
        .modalities()
        .start_voice_recording("rec-runtime", "audio/wav")
        .await
        .unwrap();
    runtime
        .modalities()
        .stream_voice_audio(voice_chunk("rec-runtime", true))
        .await
        .unwrap();
    runtime
        .modalities()
        .open_voice_playback("play-runtime")
        .await
        .unwrap();
    runtime
        .modalities()
        .append_voice_playback_chunk(playback_chunk("play-runtime"))
        .await
        .unwrap();
    runtime
        .modalities()
        .set_voice_playback_status("play-runtime", VoicePlaybackStatus::Playing)
        .await
        .unwrap();
    runtime
        .modalities()
        .record_desktop_broadcast("desktop-1", "window.changed", json!({"title": "Main"}))
        .await
        .unwrap();

    let recording = runtime
        .modalities()
        .current_voice_recording()
        .await
        .unwrap()
        .unwrap();
    let playback = runtime
        .modalities()
        .current_voice_playback_segment()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(recording.audio, vec![82, 73, 70, 70]);
    assert_eq!(playback.playback_id, "play-runtime");
    assert!(
        runtime
            .modalities()
            .latest_desktop_broadcast()
            .await
            .unwrap()
            .is_some()
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_rejects_commands_after_shutdown() {
    let runtime = AppCoreRuntime::start(AppCore::in_memory().unwrap()).unwrap();
    runtime.shutdown().await.unwrap();

    let error = runtime
        .frontend()
        .update_canvas(canvas_patch())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("frontend"));
    assert!(error.to_string().contains("running worker"));
}

fn app_with_slow_llm() -> AppCore {
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(SlowFakeProvider)]).unwrap();
    AppCore::in_memory_with_llm_registry(registry).unwrap()
}

fn canvas_patch() -> CanvasPatch {
    CanvasPatch {
        canvas_id: "main".to_string(),
        elements: vec![CanvasElement {
            element_id: "node-1".to_string(),
            element_kind: "note".to_string(),
            content: json!({"text": "hello"}),
        }],
    }
}

fn llm_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
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

fn voice_chunk(recording_id: &str, final_chunk: bool) -> VoiceAudioChunk {
    VoiceAudioChunk {
        recording_id: recording_id.to_string(),
        media_type: "audio/wav".to_string(),
        bytes: vec![82, 73, 70, 70],
        final_chunk,
    }
}

fn playback_chunk(playback_id: &str) -> VoicePlaybackChunk {
    VoicePlaybackChunk {
        playback_id: playback_id.to_string(),
        media_type: "audio/pcm;rate=24000;format=s16le".to_string(),
    }
}
