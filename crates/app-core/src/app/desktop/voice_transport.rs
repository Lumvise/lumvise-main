//! Plugin-neutral desktop voice and capability transport.

use super::*;
use lumvise_plugin_package::ExportSurface;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::PluginInvocationRequest as RuntimePluginInvocationRequest;

impl AppCoreDesktopBridge {
    fn is_renderer_command(&self, plugin_id: &str, capability_id: &str) -> Result<bool, String> {
        let installed = self
            .app
            .plugin_system()
            .cataloged_plugins()
            .map_err(|error| error.to_string())?;
        Ok(installed.iter().any(|plugin| {
            plugin.plugin_id == plugin_id
                && plugin.exports.iter().any(|export| {
                    export.id == capability_id && matches!(export.surface, ExportSurface::Command)
                })
        }))
    }

    fn invoke_renderer_command(
        &self,
        plugin_id: String,
        capability_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let context = renderer_command_context(&plugin_id, &capability_id);
        let request =
            RuntimePluginInvocationRequest::new(&plugin_id, &capability_id, input, context);
        match self
            .app
            .plugin_system()
            .invoke_controlled(request)
            .map_err(|error| error.to_string())?
        {
            WireOutcome::Succeeded { value } => Ok(value),
            WireOutcome::Failed { error } => Err(format!(
                "Renderer command {plugin_id}/{capability_id} failed ({}): {}",
                error.code, error.message,
            )),
        }
    }
}

fn renderer_command_context(
    plugin: &str,
    export: &str,
) -> lumvise_plugin_runtime::PluginInvocationContext {
    let context = crate::plugin::surface_invocation_context(
        "desktop-renderer-command",
        Instant::now() + Duration::from_secs(60),
    );
    if (plugin == "builtin.knowledge" && export == "inspect_document_space")
        || (plugin == "builtin.assistant"
            && matches!(
                export,
                "send_assistant_message" | "list_assistant_inbox" | "mark_assistant_message_read"
            ))
    {
        return context.with_human_interaction();
    }
    context
}

#[cfg(test)]
mod inspection_origin_tests {
    use super::renderer_command_context;

    #[test]
    fn only_native_inspection_and_passive_inbox_commands_receive_human_authority() {
        assert!(
            renderer_command_context("builtin.knowledge", "inspect_document_space")
                .is_human_interaction()
        );
        for export in [
            "send_assistant_message",
            "list_assistant_inbox",
            "mark_assistant_message_read",
        ] {
            assert!(renderer_command_context("builtin.assistant", export).is_human_interaction());
        }
        for (plugin, export) in [
            ("builtin.assistant", "start_assistant_session"),
            ("builtin.assistant", "receive_inbox_message"),
            ("builtin.agents", "inspect_document_space"),
            ("builtin.knowledge", "get_document"),
        ] {
            assert!(!renderer_command_context(plugin, export).is_human_interaction());
        }
    }
}

impl DesktopVoiceBridge for PendingDesktopBridge {
    fn install_host_extension(
        &self,
        name: &str,
        service: std::sync::Arc<dyn lumvise_frontend_core::HostExtension>,
    ) -> Result<(), String> {
        self.with_ready("install_host_extension", |delegate| {
            delegate.install_host_extension(name, service)
        })
    }
    fn send_live_audio(&self, input: lumvise_frontend_core::LiveAudioInput) -> Result<(), String> {
        self.with_ready("send_live_audio", |delegate| {
            delegate.send_live_audio(input)
        })
    }
    fn invoke_plugin_capability(
        &self,
        plugin_id: String,
        capability_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("invoke_plugin_capability", |delegate| {
            delegate.invoke_plugin_capability(plugin_id, capability_id, input)
        })
    }

    fn submit_voice_snippet(
        &self,
        snippet: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("submit_voice_snippet", |delegate| {
            delegate.submit_voice_snippet(snippet)
        })
    }

    fn transcribe_audio(
        &self,
        request_id: String,
        audio: Vec<f32>,
        sample_rate: u32,
        options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("transcribe_audio", |delegate| {
            delegate.transcribe_audio(request_id, audio, sample_rate, options)
        })
    }

    fn synthesize_speech(
        &self,
        request_id: String,
        text: String,
        voice: Option<String>,
        options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("synthesize_speech", |delegate| {
            delegate.synthesize_speech(request_id, text, voice, options)
        })
    }

    fn stream_synthesize_speech(
        &self,
        request_id: String,
        text: String,
        voice: Option<String>,
        on_event: &mut DesktopSpeechStreamEventSink<'_>,
    ) -> Result<(), String> {
        self.with_ready("stream_synthesize_speech", |delegate| {
            delegate.stream_synthesize_speech(request_id, text, voice, on_event)
        })
    }

    fn subscribe_voice_playback(
        &self,
        on_event: &mut DesktopVoicePlaybackEventSink<'_>,
    ) -> Result<(), String> {
        let delegate = self.wait_for_ready_delegate("subscribe_voice_playback")?;
        delegate.subscribe_voice_playback(on_event)
    }

    fn set_voice_playback_status(&self, playback_id: String, status: String) -> Result<(), String> {
        self.with_ready("set_voice_playback_status", |delegate| {
            delegate.set_voice_playback_status(playback_id, status)
        })
    }

    fn record_screen_frame_broadcast(
        &self,
        frame_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        self.with_ready("record_screen_frame_broadcast", |delegate| {
            delegate.record_screen_frame_broadcast(frame_id, media_type, bytes)
        })
    }
}

impl DesktopVoiceBridge for AppCoreDesktopBridge {
    fn install_host_extension(
        &self,
        name: &str,
        service: std::sync::Arc<dyn lumvise_frontend_core::HostExtension>,
    ) -> Result<(), String> {
        self.app.install_host_extension(name, service);
        Ok(())
    }
    fn send_live_audio(&self, input: lumvise_frontend_core::LiveAudioInput) -> Result<(), String> {
        self.app.plugin_host_services.send_audio_input(input)
    }
    fn invoke_plugin_capability(
        &self,
        plugin_id: String,
        capability_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        if self.is_renderer_command(&plugin_id, &capability_id)? {
            return self.invoke_renderer_command(plugin_id, capability_id, input);
        }
        let response = self
            .app
            .plugin_endpoints()
            .invoke_plugin_mcp_tool(PluginInvocationRequest {
                tool_name: format!("app_plugin.{plugin_id}.{capability_id}"),
                arguments: input,
            })
            .map_err(|error| error.to_string())?;
        Ok(response.output)
    }

    fn submit_voice_snippet(
        &self,
        snippet: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let purpose = snippet
            .get("purpose")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("final")
            .to_string();
        if purpose != "final" && purpose != "vad" {
            return Ok(incomplete_voice_snippet_result());
        }
        self.transcribe_snippet(snippet, purpose == "final")
    }

    fn transcribe_audio(
        &self,
        request_id: String,
        audio: Vec<f32>,
        sample_rate: u32,
        options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let response = self.transcribe_samples(audio, sample_rate)?;
        Ok(transcription_result(
            request_id,
            response.transcript,
            options,
        ))
    }

    fn synthesize_speech(
        &self,
        _request_id: String,
        text: String,
        voice: Option<String>,
        _options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let response = self.synthesize_text(&Text2VoiceRequest {
            text: speech_text_for_tts(&text),
            voice_id: voice,
            model: None,
        })?;
        Ok(json!({ "audio": response.audio, "mimeType": response.media_type }))
    }

    fn stream_synthesize_speech(
        &self,
        _request_id: String,
        text: String,
        voice: Option<String>,
        on_event: &mut DesktopSpeechStreamEventSink<'_>,
    ) -> Result<(), String> {
        self.stream_synthesize_text(
            &Text2VoiceRequest {
                text: speech_text_for_tts(&text),
                voice_id: voice,
                model: None,
            },
            on_event,
        )
    }

    fn subscribe_voice_playback(
        &self,
        on_event: &mut DesktopVoicePlaybackEventSink<'_>,
    ) -> Result<(), String> {
        let receiver = self
            .app
            .plugin_host_services
            .voice_playback_transport()
            .subscribe();
        while let Ok(event) = receiver.recv() {
            on_event(desktop_playback_event(event))?;
        }
        Ok(())
    }

    fn set_voice_playback_status(&self, playback_id: String, status: String) -> Result<(), String> {
        let parsed = parse_voice_playback_status(&status)?;
        let owner = self
            .app
            .plugin_host_services
            .report_voice_playback_status(&playback_id, parsed)
            .map_err(|error| error.to_string())?;
        if let Some((plugin_id, arguments)) =
            owner.and_then(|owner| playback_status_notification(&playback_id, parsed, owner))
        {
            // Best-effort notification must not fail the renderer's own status acknowledgement.
            let _ = self
                .app
                .plugin_endpoints()
                .invoke_plugin_mcp_tool(PluginInvocationRequest {
                    tool_name: format!("app_plugin.{plugin_id}.advance_assistant_state"),
                    arguments,
                });
        }
        Ok(())
    }

    fn record_screen_frame_broadcast(
        &self,
        frame_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        #[cfg(feature = "assistant-e2e")]
        if media_type == crate::app::e2e_control::E2E_RENDERER_EVENT_MEDIA_TYPE {
            return crate::app::e2e_control::record_encoded_renderer_event(
                &self.app, &frame_id, &bytes,
            )
            .map_err(|error| error.to_string());
        }
        let record = self
            .app
            .modalities()
            .record_screen_frame_broadcast(&frame_id, &media_type, bytes)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(record).map_err(|error| error.to_string())
    }
}

impl AppCoreDesktopBridge {
    fn stream_synthesize_text(
        &self,
        request: &Text2VoiceRequest,
        on_event: &mut DesktopSpeechStreamEventSink<'_>,
    ) -> Result<(), String> {
        let service = self.app.text2voice_service().ok_or_else(|| {
            AppCoreError::unsupported("text2voice", "configured TTS service").to_string()
        })?;
        service
            .stream_with_events(request, &InvocationControl::sixty_seconds(), &mut |event| {
                on_event(desktop_speech_event(event)).map_err(|message| {
                    NeuralError::ProviderFailed {
                        provider_id: "desktop-tts-channel".to_string(),
                        message,
                    }
                })
            })
            .map_err(|error| error.to_string())
    }

    fn transcribe_snippet(
        &self,
        snippet: serde_json::Value,
        completed: bool,
    ) -> Result<serde_json::Value, String> {
        let request_id = snippet
            .get("requestId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("voice-snippet")
            .to_string();
        let response = self.transcribe_samples(
            samples_from_value(&snippet)?,
            sample_rate_from_value(&snippet)?,
        )?;
        Ok(voice_snippet_result(
            request_id,
            response.transcript,
            completed,
        ))
    }

    fn transcribe_samples(
        &self,
        samples: Vec<f32>,
        sample_rate: u32,
    ) -> Result<lumvise_neural_core::voice2text::Voice2TextResponse, String> {
        let service = self.app.voice2text_service().ok_or_else(|| {
            AppCoreError::unsupported("voice2text", "configured STT service").to_string()
        })?;
        service
            .transcribe(
                &Voice2TextRequest {
                    audio: encode_pcm16_wav(samples, sample_rate)?,
                    media_type: "audio/wav".to_string(),
                    model: None,
                },
                &InvocationControl::sixty_seconds(),
            )
            .map_err(|error| error.to_string())
    }

    fn synthesize_text(
        &self,
        request: &Text2VoiceRequest,
    ) -> Result<lumvise_neural_core::text2voice::Text2VoiceResponse, String> {
        let service = self.app.text2voice_service().ok_or_else(|| {
            AppCoreError::unsupported("text2voice", "configured TTS service").to_string()
        })?;
        service
            .synthesize(request, &InvocationControl::sixty_seconds())
            .map_err(|error| error.to_string())
    }
}

fn desktop_speech_event(event: Text2VoiceStreamEvent) -> DesktopSpeechStreamEvent {
    match event {
        Text2VoiceStreamEvent::AudioChunk {
            sequence,
            audio,
            media_type,
        } => DesktopSpeechStreamEvent::AudioChunk {
            sequence,
            audio,
            mime_type: media_type,
        },
        Text2VoiceStreamEvent::Complete => DesktopSpeechStreamEvent::Complete,
        Text2VoiceStreamEvent::Error { message } => DesktopSpeechStreamEvent::Error { message },
    }
}

fn desktop_playback_event(
    event: crate::plugin::VoicePlaybackTransportEvent,
) -> DesktopVoicePlaybackEvent {
    use crate::plugin::VoicePlaybackTransportEvent as Transport;
    match event {
        Transport::Opened {
            playback_id,
            media_type,
            sample_rate_hz,
        } => DesktopVoicePlaybackEvent::Opened {
            playback_id,
            media_type,
            sample_rate_hz,
        },
        Transport::AudioChunk {
            playback_id,
            sequence,
            audio,
        } => DesktopVoicePlaybackEvent::AudioChunk {
            playback_id,
            sequence,
            audio,
        },
        Transport::Closed { playback_id } => DesktopVoicePlaybackEvent::Closed { playback_id },
        Transport::Cancelled { playback_id } => {
            DesktopVoicePlaybackEvent::Cancelled { playback_id }
        }
    }
}

fn parse_voice_playback_status(status: &str) -> Result<VoicePlaybackStatus, String> {
    match status {
        "queued" => Ok(VoicePlaybackStatus::Queued),
        "playing" => Ok(VoicePlaybackStatus::Playing),
        "completed" => Ok(VoicePlaybackStatus::Completed),
        "cancelled" => Ok(VoicePlaybackStatus::Cancelled),
        "failed" => Ok(VoicePlaybackStatus::Failed),
        other => Err(format!("unknown voice playback status: {other}")),
    }
}

/// Maps a real renderer-reported playback status into the generic advance
/// event App Core notifies the owning plugin/session with (W2). `Queued`
/// is not a session transition; `Failed` reports unavailable audio without
/// discarding the generated answer or ending the conversation.
fn advance_event_for_status(status: VoicePlaybackStatus) -> Option<&'static str> {
    match status {
        VoicePlaybackStatus::Playing => Some("playback_started"),
        VoicePlaybackStatus::Completed => Some("playback_completed"),
        VoicePlaybackStatus::Cancelled => Some("playback_interrupted"),
        VoicePlaybackStatus::Failed => Some("playback_unavailable"),
        VoicePlaybackStatus::Queued => None,
    }
}

fn playback_status_notification(
    playback_id: &str,
    status: VoicePlaybackStatus,
    owner: crate::plugin::PlaybackOwner,
) -> Option<(String, serde_json::Value)> {
    let event = if owner.failure.is_some() {
        "playback_unavailable"
    } else {
        advance_event_for_status(status)?
    };
    let mut arguments = json!({"session_id":owner.session_id,"playback_id":playback_id,
        "event":event,"interrupted_at_segment":owner.segment_index});
    if let Some(error) = playback_status_error(status, owner.failure) {
        arguments["error"] = json!(error);
    }
    Some((owner.plugin_id, arguments))
}

fn playback_status_error(
    status: VoicePlaybackStatus,
    synthesis_failure: Option<String>,
) -> Option<String> {
    synthesis_failure.or_else(|| {
        (status == VoicePlaybackStatus::Failed)
            .then(|| "Audio playback failed; the response remains available as text.".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn synthesis_error_notifies_unavailable_audio_instead_of_interruption() {
        let owner = crate::plugin::PlaybackOwner {
            plugin_id: "builtin.assistant".into(),
            session_id: "workspace:test".into(),
            segment_index: 2,
            failure: Some("Speech generation failed: HTTP 502".into()),
        };
        let (plugin, arguments) =
            playback_status_notification("turn-2", VoicePlaybackStatus::Cancelled, owner).unwrap();
        assert_eq!(plugin, "builtin.assistant");
        assert_eq!(
            arguments,
            json!({"session_id":"workspace:test","playback_id":"turn-2","event":"playback_unavailable","interrupted_at_segment":2,"error":"Speech generation failed: HTTP 502"})
        );
    }

    #[test]
    fn ordinary_interruption_has_no_error_or_speech_failure_notification() {
        let owner = crate::plugin::PlaybackOwner {
            plugin_id: "builtin.assistant".into(),
            session_id: "floating".into(),
            segment_index: 0,
            failure: None,
        };
        let (_, arguments) =
            playback_status_notification("turn-1", VoicePlaybackStatus::Cancelled, owner).unwrap();
        assert_eq!(arguments["event"], "playback_interrupted");
        assert!(arguments.get("error").is_none());
    }

    #[test]
    fn desktop_speech_event_preserves_audio_chunk_contract() {
        let event = desktop_speech_event(Text2VoiceStreamEvent::AudioChunk {
            sequence: 3,
            audio: vec![82, 73, 70, 70],
            media_type: "audio/wav".to_string(),
        });

        assert_eq!(
            event,
            DesktopSpeechStreamEvent::AudioChunk {
                sequence: 3,
                audio: vec![82, 73, 70, 70],
                mime_type: "audio/wav".to_string(),
            }
        );
        assert_eq!(
            serde_json::to_value(&event).unwrap()["mimeType"],
            "audio/wav"
        );
    }

    #[test]
    fn desktop_playback_event_uses_renderer_field_names() {
        let event = desktop_playback_event(crate::plugin::VoicePlaybackTransportEvent::Opened {
            playback_id: "playback-1".to_string(),
            media_type: "audio/pcm;rate=24000".to_string(),
            sample_rate_hz: Some(24_000),
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "type": "opened",
                "playbackId": "playback-1",
                "mimeType": "audio/pcm;rate=24000",
                "sampleRateHz": 24_000,
            })
        );
    }

    /// Reproduces the startup race: the renderer's process-lifetime
    /// subscription can legitimately start before App Core finishes
    /// installing the real `AppCoreDesktopBridge`. It must block (not
    /// fail fast) until `install` runs, then observe events published
    /// through the now-ready delegate with no missed wakeup.
    #[test]
    fn subscribe_voice_playback_waits_for_startup_install_then_observes_event() {
        let app = Arc::new(AppCore::in_memory().unwrap());
        let bridge = Arc::new(PendingDesktopBridge::new());

        let (event_tx, event_rx) = mpsc::channel::<DesktopVoicePlaybackEvent>();
        let (done_tx, done_rx) = mpsc::channel::<Result<(), String>>();

        let subscriber = Arc::clone(&bridge);
        thread::spawn(move || {
            let outcome = subscriber.subscribe_voice_playback(&mut |event| {
                let _ = event_tx.send(event);
                // Terminate the process-lifetime loop deterministically
                // once the one expected event has crossed the bridge,
                // instead of blocking the test on a subscription that by
                // design never returns on its own.
                Err("regression test: stop after observing one event".to_string())
            });
            let _ = done_tx.send(outcome);
        });

        // Startup has not installed a delegate yet: the subscription must
        // stay pending rather than fail fast or deliver anything.
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let delegate = Arc::new(AppCoreDesktopBridge::without_runtime(app.clone()));
        bridge
            .install(delegate)
            .expect("installing the pending bridge delegate wakes the waiting subscription");

        app.plugin_host_services
            .voice_playback_transport()
            .publish(crate::plugin::VoicePlaybackTransportEvent::Opened {
                playback_id: "playback-1".to_string(),
                media_type: "audio/pcm;rate=24000".to_string(),
                sample_rate_hz: Some(24_000),
            })
            .expect("publishing the transport event after install");

        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("subscription observes the event once the real bridge is installed");
        assert_eq!(
            event,
            DesktopVoicePlaybackEvent::Opened {
                playback_id: "playback-1".to_string(),
                media_type: "audio/pcm;rate=24000".to_string(),
                sample_rate_hz: Some(24_000),
            }
        );

        let outcome = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("subscriber thread terminates once its callback errors");
        assert_eq!(
            outcome,
            Err("regression test: stop after observing one event".to_string())
        );
    }
}

fn speech_text_for_tts(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
