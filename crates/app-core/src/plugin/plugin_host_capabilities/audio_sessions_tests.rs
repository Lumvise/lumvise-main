//! Exercises the host capability and desktop frame public entrypoints together.
use super::*;
use crate::workspace_activity::{ActivityKind, ActivityStatus};
use lumvise_frontend_core::{AppSettingsPatch, AssistantEngine, LiveAudioInput, WorkArea};
use lumvise_neural_core::llm_providers::contract::LlmProvider;
use lumvise_neural_core::llm_providers::{
    AudioSessionEventSink, AudioSessionInput, LlmCapabilitySupport, LlmProviderCapabilities,
    LlmResponse,
};
use std::time::Duration;

struct FakeDuplexProvider;
impl LlmProvider for FakeDuplexProvider {
    fn provider_id(&self) -> &str {
        "gemini"
    }
    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: "gemini".into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Supported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Supported,
        }
    }
    fn complete(&self, _: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        panic!("direct audio must not complete text")
    }
    fn stream_with_events(
        &self,
        _: &LlmRequest,
        _: lumvise_neural_core::process::StreamControl,
        _: &mut lumvise_neural_core::llm_providers::contract::LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        panic!("direct audio must not stream text")
    }
    fn run_audio_session(
        &self,
        request: AudioSessionRequest,
        input: AudioSessionInput,
        sink: &mut AudioSessionEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        if request
            .conversation
            .messages
            .iter()
            .any(|message| message.content == "fail")
        {
            return Err(lumvise_neural_core::NeuralError::ProviderFailed {
                provider_id: self.provider_id().into(),
                message: "fixture direct audio failure".into(),
            });
        }
        sink(AudioSessionEvent::Ready {
            provider_session_id: Some("provider-session".into()),
        })?;
        while let Ok(command) = input.recv() {
            match command {
                AudioSessionCommand::Pcm(pcm) => {
                    sink(AudioSessionEvent::ResponseStarted {
                        response_id: "r1".into(),
                    })?;
                    sink(AudioSessionEvent::Audio {
                        response_id: "r1".into(),
                        item_id: Some("item1".into()),
                        pcm,
                        sample_rate: 24000,
                    })?;
                    sink(AudioSessionEvent::ResponseFinished {
                        response_id: "r1".into(),
                    })?;
                }
                AudioSessionCommand::PlaybackStopped { played_ms, item_id } => {
                    sink(AudioSessionEvent::Transcript {
                        item_id: Some(item_id),
                        text: played_ms.to_string(),
                        final_revision: true,
                    })?
                }
                AudioSessionCommand::Close => break,
                _ => {}
            }
        }
        Ok(())
    }
}

fn audio_services() -> Arc<PluginHostServices> {
    let mut frontend = FrontendCore::default();
    frontend
        .spawn_app(WorkArea::new(0, 0, 1440, 900, 1.0))
        .unwrap();
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantEngine(AssistantEngine::Gemini));
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantDirectAudio(true));
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(FakeDuplexProvider)]).unwrap();
    PluginHostServices::new(
        frontend,
        registry,
        Arc::new(lumvise_db_core::LocalPersistence::in_memory().expect("test persistence")),
    )
}
fn call_audio(host: &PluginHostServices, input: Value) -> Value {
    host.invoke("builtin.assistant", AUDIO_SESSION, input)
        .unwrap()
}
fn prepare(host: &PluginHostServices) {
    prepare_with_text(host, "Explain");
}
fn prepare_with_text(host: &PluginHostServices, text: &str) {
    call_audio(
        host,
        json!({"operation":"prepare","session_id":"audio-test","session_epoch":7,"listen_first":true,
        "request":{"messages":[{"role":"user","content":text}],"mcp_servers":[]}}),
    );
}
fn wait_event(host: &PluginHostServices, kind: &str) -> Value {
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        let output = call_audio(host, json!({"operation":"poll","session_id":"audio-test"}));
        for event in output["events"].as_array().unwrap() {
            if event["kind"] == kind {
                return event.clone();
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("expected {kind} within 3 seconds");
}
fn microphone() -> LiveAudioInput {
    LiveAudioInput {
        session_id: "audio-test".into(),
        session_epoch: 7,
        pcm: vec![1, 0, 2, 0],
        ..Default::default()
    }
}

#[test]
fn direct_audio_session_tracks_assistant_activity_until_close() {
    let host = audio_services();
    prepare(&host);
    wait_event(&host, "ready");

    let running = host
        .activity()
        .wait_for_changes("/repo", None, Duration::ZERO);
    let running_entry = running
        .entries
        .as_ref()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.kind == ActivityKind::Assistant)
        })
        .expect("Assistant audio activity while provider session is open");
    assert_eq!(running_entry.title, "Assistant · gemini");
    assert_eq!(running_entry.status, ActivityStatus::Running);

    call_audio(
        &host,
        json!({"operation":"close","session_id":"audio-test"}),
    );
    let finished =
        host.activity()
            .wait_for_changes("/repo", Some(running.revision), Duration::from_secs(3));
    let finished_entry = finished
        .entries
        .as_ref()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.kind == ActivityKind::Assistant)
        })
        .expect("Assistant audio activity after close");
    assert_eq!(finished_entry.title, "Assistant · gemini");
    assert_eq!(finished_entry.status, ActivityStatus::Succeeded);
}

#[test]
fn direct_audio_provider_failure_marks_assistant_activity_failed() {
    let host = audio_services();
    prepare_with_text(&host, "fail");
    let failure = wait_event(&host, "error");
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|message| message.contains("fixture direct audio failure"))
    );

    let entries = host
        .activity()
        .wait_for_changes("/repo", None, Duration::ZERO)
        .entries
        .expect("failed activity snapshot");
    assert!(entries.iter().any(|entry| {
        entry.kind == ActivityKind::Assistant
            && entry.title == "Assistant · gemini"
            && entry.status == ActivityStatus::Failed
            && entry
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("fixture direct audio failure"))
    }));
}

#[test]
fn scoped_microphone_streams_to_renderer_and_pause_cancels_queued_playback() {
    let host = audio_services();
    let playback = host.voice_playback_transport.subscribe();
    prepare(&host);
    assert_eq!(
        wait_event(&host, "ready")["provider_session_id"],
        "provider-session"
    );
    host.send_audio_input(microphone()).unwrap();
    assert!(matches!(
        playback.recv_timeout(Duration::from_secs(3)).unwrap(),
        VoicePlaybackTransportEvent::Opened { .. }
    ));
    assert!(
        matches!(playback.recv_timeout(Duration::from_secs(3)).unwrap(), VoicePlaybackTransportEvent::AudioChunk { audio, sequence:0, .. } if audio == [1,0,2,0])
    );
    assert!(matches!(
        playback.recv_timeout(Duration::from_secs(3)).unwrap(),
        VoicePlaybackTransportEvent::Closed { .. }
    ));
    host.send_audio_input(LiveAudioInput {
        control: 1,
        pcm: vec![],
        ..microphone()
    })
    .unwrap();
    assert!(matches!(
        playback.recv_timeout(Duration::from_secs(3)).unwrap(),
        VoicePlaybackTransportEvent::Cancelled { .. }
    ));
    call_audio(
        &host,
        json!({"operation":"close","session_id":"audio-test"}),
    );
    assert!(
        host.send_audio_input(microphone())
            .unwrap_err()
            .contains("current active audio session")
    );
}
#[test]
fn stale_epoch_and_invalid_pcm_never_reach_the_provider() {
    let host = audio_services();
    prepare(&host);
    wait_event(&host, "ready");
    assert!(
        host.send_audio_input(LiveAudioInput {
            session_epoch: 6,
            ..microphone()
        })
        .unwrap_err()
        .contains("epoch 6")
    );
    assert!(
        host.send_audio_input(LiveAudioInput {
            pcm: vec![1],
            ..microphone()
        })
        .unwrap_err()
        .contains("even PCM16")
    );
    assert!(
        host.send_audio_input(LiveAudioInput {
            control: 99,
            ..microphone()
        })
        .unwrap_err()
        .contains("Audio control 99")
    );
    call_audio(
        &host,
        json!({"operation":"close","session_id":"audio-test"}),
    );
}
#[test]
fn actual_renderer_played_offset_routes_to_its_original_provider_item() {
    let host = audio_services();
    prepare(&host);
    wait_event(&host, "ready");
    host.send_audio_input(microphone()).unwrap();
    let event = wait_event(&host, "response_finished");
    host.send_audio_input(LiveAudioInput {
        control: 4,
        pcm: vec![],
        playback_id: event["playback_id"].as_str().unwrap().into(),
        played_ms: 237,
        ..microphone()
    })
    .unwrap();
    let truncated = wait_event(&host, "transcript");
    assert_eq!(truncated["item_id"], "item1");
    assert_eq!(truncated["text"], "237");
    call_audio(
        &host,
        json!({"operation":"close","session_id":"audio-test"}),
    );
}
