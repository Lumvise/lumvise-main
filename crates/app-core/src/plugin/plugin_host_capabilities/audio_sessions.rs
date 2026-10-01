//! Owns live provider connections and audio delivery. Assistant phases/history
//! remain in the Assistant plugin. Call `audio_session` for lifecycle and
//! `send_audio_input` for desktop microphone/control frames.
use super::*;
use lumvise_neural_core::llm_providers::{
    AudioSessionCommand, AudioSessionEvent, AudioSessionRequest,
};
use std::sync::mpsc::{self, Sender};

pub(super) const AUDIO_SESSION: &str = "runtime.audio_session";

#[derive(Default)]
pub(super) struct LiveAudioConnections {
    sessions: Mutex<HashMap<String, LiveAudioConnection>>,
}

struct LiveAudioConnection {
    epoch: u64,
    input: Sender<AudioSessionCommand>,
    projection: Arc<Mutex<LiveAudioProjection>>,
}

#[derive(Default)]
struct LiveAudioProjection {
    events: Vec<Value>,
    items: HashMap<String, String>,
    playback_id: Option<String>,
    sequence: u64,
    closed: bool,
    opened: bool,
}

impl PluginHostServices {
    pub(super) fn audio_session(&self, input: Value) -> Result<Value, HostCapabilityError> {
        let session_id = input["session_id"].as_str().unwrap_or_default();
        match input["operation"].as_str() {
            Some("selection") => self.audio_selection(),
            Some("prepare") => self.prepare_audio(input),
            Some("poll") => self.poll_audio(session_id),
            Some("close") => self.close_audio(session_id),
            Some("pause") => self.send_audio_control(session_id, AudioSessionCommand::Pause),
            Some("resume") => self.send_audio_control(session_id, AudioSessionCommand::Resume),
            Some("interrupt") => self.interrupt_audio(session_id),
            Some("text") => self.send_audio_control(
                session_id,
                AudioSessionCommand::Text(input["text"].as_str().unwrap_or_default().into()),
            ),
            _ => Err(invalid(
                AUDIO_SESSION,
                &input,
                "audio session operation selection, prepare, poll, close, pause, resume, interrupt, or text",
            )),
        }
    }

    fn audio_selection(&self) -> Result<Value, HostCapabilityError> {
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "frontend lock poisoned"))?;
        let settings = frontend.app_settings();
        if !settings.assistant_direct_audio {
            return Ok(json!({"enabled":false}));
        }
        let provider = settings.assistant_engine.value();
        if !matches!(provider, "openai_realtime" | "gemini") {
            return Err(invalid(
                AUDIO_SESSION,
                &json!(provider),
                "OpenAI Realtime or Gemini Live for direct audio",
            ));
        }
        Ok(json!({"enabled":true,"provider_id":provider,"model":settings.assistant_model}))
    }

    fn prepare_audio(&self, input: Value) -> Result<Value, HostCapabilityError> {
        let selection = self.audio_selection()?;
        if selection["enabled"] != true {
            return Ok(selection);
        }
        let session_id = required_audio_string(&input, "session_id")?;
        let epoch = input["session_epoch"]
            .as_u64()
            .ok_or_else(|| invalid(AUDIO_SESSION, &input, "session_epoch integer"))?;
        let mut request: NeutralLlmRequest = decode(AUDIO_SESSION, input["request"].clone())?;
        request.validate()?;
        let provider_id = resolve_llm_selection(&self.frontend, AUDIO_SESSION, &mut request)?;
        let provider = self
            .llms
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "provider registry lock poisoned"))?
            .provider_handle(&provider_id)
            .map_err(|error| failed(AUDIO_SESSION, &error.to_string()))?;
        let request = AudioSessionRequest {
            conversation: request.into_neural(),
            voice: input["voice"].as_str().map(str::to_owned),
        };
        self.launch_audio(
            session_id,
            epoch,
            provider,
            request,
            input["listen_first"] == true,
        )?;
        Ok(selection)
    }

    fn launch_audio(
        &self,
        session_id: String,
        epoch: u64,
        provider: Arc<dyn lumvise_neural_core::llm_providers::contract::LlmProvider>,
        request: AudioSessionRequest,
        listen_first: bool,
    ) -> Result<(), HostCapabilityError> {
        let mut sessions = self
            .audio_connections
            .sessions
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio connections lock poisoned"))?;
        if sessions.contains_key(&session_id) {
            return Err(invalid(
                AUDIO_SESSION,
                &json!(session_id),
                "new audio session id",
            ));
        }
        let ticket = self
            .activity
            .track_llm("builtin.assistant", provider.provider_id());
        let (sender, receiver) = mpsc::channel();
        let projection = Arc::new(Mutex::new(LiveAudioProjection::default()));
        sessions.insert(
            session_id.clone(),
            LiveAudioConnection {
                epoch,
                input: sender.clone(),
                projection: Arc::clone(&projection),
            },
        );
        let transport = Arc::clone(&self.voice_playback_transport);
        std::thread::spawn(move || {
            let opening = request
                .conversation
                .messages
                .iter()
                .filter(|message| message.role == "user")
                .map(|message| message.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            if !listen_first {
                let _ = sender.send(AudioSessionCommand::Text(opening));
            }
            ticket.started();
            let outcome = provider.run_audio_session(request, receiver, &mut |event| {
                deliver_audio_event(&session_id, epoch, event, &projection, &transport).map_err(
                    |error| lumvise_neural_core::NeuralError::ProviderFailed {
                        provider_id: "assistant-audio".into(),
                        message: error.to_string(),
                    },
                )
            });
            ticket.finish_stream(&outcome);
            if let Ok(mut state) = projection.lock() {
                cancel_audio_projection(&mut state, &transport);
                state.closed = true;
                if let Err(error) = outcome {
                    state
                        .events
                        .push(json!({"kind":"error","message":error.to_string()}));
                }
            }
        });
        Ok(())
    }

    fn poll_audio(&self, session_id: &str) -> Result<Value, HostCapabilityError> {
        let sessions = self
            .audio_connections
            .sessions
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio connections lock poisoned"))?;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| invalid(AUDIO_SESSION, &json!(session_id), "prepared audio session"))?;
        let mut projection = session
            .projection
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio projection lock poisoned"))?;
        Ok(json!({"events":std::mem::take(&mut projection.events),"closed":projection.closed}))
    }

    fn close_audio(&self, session_id: &str) -> Result<Value, HostCapabilityError> {
        let session = self
            .audio_connections
            .sessions
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio connections lock poisoned"))?
            .remove(session_id);
        if let Some(session) = session {
            let _ = session.input.send(AudioSessionCommand::Close);
            if let Ok(mut projection) = session.projection.lock() {
                projection.closed = true;
                cancel_audio_projection(&mut projection, &self.voice_playback_transport);
            }
        }
        Ok(json!({"closed":true}))
    }

    fn send_audio_control(
        &self,
        session_id: &str,
        command: AudioSessionCommand,
    ) -> Result<Value, HostCapabilityError> {
        let sessions = self
            .audio_connections
            .sessions
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio connections lock poisoned"))?;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| invalid(AUDIO_SESSION, &json!(session_id), "active audio session"))?;
        if matches!(command, AudioSessionCommand::Pause) {
            let mut projection = session
                .projection
                .lock()
                .map_err(|_| failed(AUDIO_SESSION, "audio projection lock poisoned"))?;
            cancel_audio_projection(&mut projection, &self.voice_playback_transport);
        }
        session
            .input
            .send(command)
            .map_err(|_| failed(AUDIO_SESSION, "audio connection ended"))?;
        Ok(json!({"accepted":true}))
    }

    fn interrupt_audio(&self, session_id: &str) -> Result<Value, HostCapabilityError> {
        let sessions = self
            .audio_connections
            .sessions
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio connections lock poisoned"))?;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| invalid(AUDIO_SESSION, &json!(session_id), "active audio session"))?;
        let mut projection = session
            .projection
            .lock()
            .map_err(|_| failed(AUDIO_SESSION, "audio projection lock poisoned"))?;
        let _ = session.input.send(AudioSessionCommand::Interrupt {
            item_id: None,
            played_ms: 0,
        });
        cancel_audio_projection(&mut projection, &self.voice_playback_transport);
        Ok(json!({"accepted":true}))
    }

    /// Accepts PCM16LE/24kHz only for the active session epoch. Example: microphone frame → this boundary.
    pub(crate) fn send_audio_input(
        &self,
        input: lumvise_frontend_core::LiveAudioInput,
    ) -> Result<(), String> {
        let lumvise_frontend_core::LiveAudioInput {
            session_id,
            session_epoch: epoch,
            pcm,
            control,
            played_ms,
            playback_id,
        } = input;
        let session_id = session_id.as_str();
        if control == 0 && (pcm.is_empty() || pcm.len() % 2 != 0) {
            return Err(format!(
                "PCM byte count {}; expected non-empty even PCM16 byte count",
                pcm.len()
            ));
        }
        let sessions = self
            .audio_connections
            .sessions
            .lock()
            .map_err(|_| "audio connections lock poisoned".to_string())?;
        let session=sessions.get(session_id).filter(|session| session.epoch==epoch)
            .ok_or_else(|| format!("audio session {session_id:?} epoch {epoch}; expected current active audio session"))?;
        let command = match control {
            0 => AudioSessionCommand::Pcm(pcm),
            1 => {
                drop(sessions);
                return self
                    .send_audio_control(session_id, AudioSessionCommand::Pause)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
            }
            2 => AudioSessionCommand::Resume,
            3 => {
                drop(sessions);
                return self
                    .interrupt_audio(session_id)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
            }
            4 => {
                let projection = session
                    .projection
                    .lock()
                    .map_err(|_| "audio projection lock poisoned".to_string())?;
                let Some(item_id) = projection.items.get(&playback_id).cloned() else {
                    return Ok(());
                };
                AudioSessionCommand::PlaybackStopped { item_id, played_ms }
            }
            5 => {
                session
                    .projection
                    .lock()
                    .map_err(|_| "audio projection lock poisoned".to_string())?
                    .events
                    .push(json!({"kind":"input_stopped"}));
                AudioSessionCommand::UserTurnEnded
            }
            other => {
                return Err(format!(
                    "Audio control {other}; expected 0 PCM, 1 pause, 2 resume, 3 interrupt, 4 playback stopped, or 5 utterance ended"
                ));
            }
        };
        session
            .input
            .send(command)
            .map_err(|_| "audio connection ended".to_string())
    }

    pub(super) fn report_audio_playback(
        &self,
        playback_id: &str,
        status: VoicePlaybackStatus,
    ) -> bool {
        let Ok(sessions) = self.audio_connections.sessions.lock() else {
            return false;
        };
        for session in sessions.values() {
            let Ok(mut projection) = session.projection.lock() else {
                continue;
            };
            if projection.playback_id.as_deref() != Some(playback_id) {
                continue;
            }
            projection
                .events
                .push(json!({"kind":"playback_status","playback_id":playback_id,"status":status}));
            return true;
        }
        false
    }
}

fn required_audio_string(input: &Value, key: &str) -> Result<String, HostCapabilityError> {
    input[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(AUDIO_SESSION, input, &format!("non-empty {key}")))
}

fn deliver_audio_event(
    session_id: &str,
    epoch: u64,
    event: AudioSessionEvent,
    projection: &Mutex<LiveAudioProjection>,
    transport: &super::super::voice_playback_transport::VoicePlaybackTransport,
) -> Result<(), HostCapabilityError> {
    let mut state = projection
        .lock()
        .map_err(|_| failed(AUDIO_SESSION, "audio projection lock poisoned"))?;
    if state.closed {
        return Ok(());
    }
    if let AudioSessionEvent::Audio {
        pcm,
        sample_rate,
        item_id,
        ..
    } = event
    {
        return publish_audio_chunk(&mut state, pcm, sample_rate, item_id, transport);
    }
    match &event {
        AudioSessionEvent::ResponseStarted { response_id } => {
            close_audio_projection(&mut state, transport)?;
            state.playback_id = Some(format!(
                "assistant-playback-{session_id}:{epoch}:{response_id}"
            ));
            state.sequence = 0;
            state.opened = false;
        }
        AudioSessionEvent::ResponseFinished { .. } => {
            close_audio_projection(&mut state, transport)?
        }
        AudioSessionEvent::Interrupted => {
            cancel_audio_projection(&mut state, transport);
        }
        _ => {}
    }
    let mut value =
        serde_json::to_value(event).map_err(|error| failed(AUDIO_SESSION, &error.to_string()))?;
    value["playback_id"] = json!(state.playback_id);
    state.events.push(value);
    Ok(())
}

fn publish_audio_chunk(
    state: &mut LiveAudioProjection,
    pcm: Vec<u8>,
    rate: u32,
    item_id: Option<String>,
    transport: &super::super::voice_playback_transport::VoicePlaybackTransport,
) -> Result<(), HostCapabilityError> {
    let playback_id = state
        .playback_id
        .clone()
        .ok_or_else(|| failed(AUDIO_SESSION, "audio arrived before response started"))?;
    if let Some(item) = &item_id {
        state.items.insert(playback_id.clone(), item.clone());
    }
    if !state.opened {
        publish(
            transport,
            VoicePlaybackTransportEvent::Opened {
                playback_id: playback_id.clone(),
                media_type: format!("audio/pcm;rate={rate};format=s16le"),
                sample_rate_hz: Some(rate),
            },
        )?;
        state.opened = true;
    }
    publish(
        transport,
        VoicePlaybackTransportEvent::AudioChunk {
            playback_id,
            sequence: state.sequence,
            audio: pcm,
        },
    )?;
    state.sequence += 1;
    Ok(())
}

fn close_audio_projection(
    state: &mut LiveAudioProjection,
    transport: &super::super::voice_playback_transport::VoicePlaybackTransport,
) -> Result<(), HostCapabilityError> {
    if state.opened {
        if let Some(playback_id) = state.playback_id.clone() {
            publish(
                transport,
                VoicePlaybackTransportEvent::Closed { playback_id },
            )?;
        }
        state.opened = false;
    }
    Ok(())
}

fn cancel_audio_projection(
    state: &mut LiveAudioProjection,
    transport: &super::super::voice_playback_transport::VoicePlaybackTransport,
) {
    if let Some(playback_id) = state.playback_id.clone() {
        let _ = publish(
            transport,
            VoicePlaybackTransportEvent::Cancelled { playback_id },
        );
    }
    state.opened = false;
}

fn publish(
    transport: &super::super::voice_playback_transport::VoicePlaybackTransport,
    event: VoicePlaybackTransportEvent,
) -> Result<(), HostCapabilityError> {
    transport
        .publish(event)
        .map_err(|error| failed(AUDIO_SESSION, &error.to_string()))
}

impl Drop for LiveAudioConnections {
    fn drop(&mut self) {
        if let Ok(sessions) = self.sessions.lock() {
            for session in sessions.values() {
                let _ = session.input.send(AudioSessionCommand::Close);
            }
        }
    }
}

#[cfg(test)]
#[path = "audio_sessions_tests.rs"]
mod audio_sessions_tests;
