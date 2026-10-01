//! Plugin-neutral desktop voice and capability transport.

use super::*;

impl DesktopVoiceBridge for PendingDesktopBridge {
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
    fn send_live_audio(&self, input: lumvise_frontend_core::LiveAudioInput) -> Result<(), String> {
        self.app.plugin_host_services.send_audio_input(input)
    }
    fn invoke_plugin_capability(
        &self,
        plugin_id: String,
        capability_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
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
        if let (Some((plugin_id, session_id, segment_index)), Some(event)) =
            (owner, advance_event_for_status(parsed))
        {
            // Best-effort: a session that has already ended or a plugin
            // that is momentarily busy must not fail the status report the
            // renderer is relying on to keep its own state correct.
            let _ = self
                .app
                .plugin_endpoints()
                .invoke_plugin_mcp_tool(PluginInvocationRequest {
                    tool_name: format!("app_plugin.{plugin_id}.advance_assistant_state"),
                    arguments: json!({
                        "session_id": session_id,
                        "playback_id": playback_id,
                        "event": event,
                        "interrupted_at_segment": segment_index,
                    }),
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
/// and `Failed` are not part of the `Speaking`/`AwaitingUser`/`Listening`
/// state machine and are intentionally not forwarded.
fn advance_event_for_status(status: VoicePlaybackStatus) -> Option<&'static str> {
    match status {
        VoicePlaybackStatus::Playing => Some("playback_started"),
        VoicePlaybackStatus::Completed => Some("playback_completed"),
        VoicePlaybackStatus::Cancelled => Some("playback_interrupted"),
        VoicePlaybackStatus::Queued | VoicePlaybackStatus::Failed => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

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
