//! Owns streamed synthesis and playback correlation for the two assistant surfaces.
//! Host capability dispatch and renderer status reports enter through PluginHostServices.

use super::*;

impl PluginHostServices {
    pub(super) fn text_to_speech(
        &self,
        plugin_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        let request: TextToSpeechRequest = decode(TEXT_TO_SPEECH, input)?;
        require_non_empty(TEXT_TO_SPEECH, &request.playback_id, "playback_id")?;
        require_non_empty(TEXT_TO_SPEECH, &request.session_id, "session_id")?;

        if request.cancel {
            return self.cancel_text_to_speech(&request.playback_id);
        }

        let text = request.text.as_deref().unwrap_or_default();
        if text.is_empty() && !request.close {
            require_non_empty(TEXT_TO_SPEECH, text, "text")?;
        }
        if text.len() > MAX_TTS_TEXT_BYTES {
            return Err(quota(TEXT_TO_SPEECH, text.len(), MAX_TTS_TEXT_BYTES));
        }

        let workspace = workspace_session(&request.session_id);
        let executor = self.text_to_speech_executor(&request.session_id)?;
        let segment_index = if text.is_empty() {
            self.current_playback_segment_index(&request.playback_id)?
        } else {
            self.open_or_append_playback_segment(&request)?
        };
        self.remember_playback_owner(
            &request.playback_id,
            plugin_id,
            &request.session_id,
            segment_index,
        );

        let synth_control = self.playback_control(&request.playback_id);
        let failure_control = synth_control.clone();
        let owners_map = Arc::clone(&self.playback_owners);

        let transport = self.voice_playback_transport();
        let playback_id = request.playback_id.clone();
        let close_after = request.close;
        let frontend = Arc::clone(&self.frontend);
        let opened_map = Arc::clone(&self.playback_opened);
        let sequences_map = Arc::clone(&self.playback_sequences);
        let controls_map = Arc::clone(&self.playback_controls);

        let enqueue_result = executor.enqueue_stream(
            Text2VoiceRequest {
                text: text.to_string(),
                voice_id: request.voice_id.clone(),
                model: request.model.clone(),
            },
            synth_control,
            move |event| {
                match event {
                    Text2VoiceStreamEvent::AudioChunk {
                        sequence,
                        audio,
                        media_type,
                    } => {
                        let first_chunk = opened_map
                            .lock()
                            .map(|mut opened| opened.insert(playback_id.clone()))
                            .unwrap_or(false);
                        if first_chunk {
                            let _ = transport.publish(VoicePlaybackTransportEvent::Opened {
                                playback_id: playback_id.clone(),
                                media_type: media_type.clone(),
                                sample_rate_hz: sample_rate_from_media_type(&media_type),
                            });
                        }
                        let transport_sequence = sequences_map
                            .lock()
                            .map(|mut sequences| {
                                let next = sequences.entry(playback_id.clone()).or_insert(0);
                                let sequence = *next;
                                *next = next.saturating_add(1);
                                sequence
                            })
                            .unwrap_or(sequence);
                        let _ = transport.publish(VoicePlaybackTransportEvent::AudioChunk {
                            playback_id: playback_id.clone(),
                            sequence: transport_sequence,
                            audio,
                        });
                    }
                    Text2VoiceStreamEvent::Complete => {
                        if close_after {
                            let _ = transport.publish(VoicePlaybackTransportEvent::Closed {
                                playback_id: playback_id.clone(),
                            });
                            if !workspace && let Ok(mut frontend) = frontend.lock() {
                                let _ = frontend.close_voice_playback(playback_id.clone());
                            }
                            cleanup_synthesis(
                                &controls_map,
                                &opened_map,
                                &sequences_map,
                                &playback_id,
                            );
                        }
                    }
                    Text2VoiceStreamEvent::Error { message } => {
                        if !record_synthesis_failure(
                            &owners_map,
                            &playback_id,
                            &failure_control,
                            &message,
                        ) {
                            return Ok(());
                        }
                        tracing::warn!(
                            event = "speech_synthesis_stream_failed",
                            %playback_id,
                            error = %message,
                        );
                        let _ = transport.publish(VoicePlaybackTransportEvent::Cancelled {
                            playback_id: playback_id.clone(),
                        });
                        if !workspace && let Ok(mut frontend) = frontend.lock() {
                            let _ = frontend.set_voice_playback_status(
                                playback_id.clone(),
                                VoicePlaybackStatus::Failed,
                            );
                        }
                        cleanup_synthesis(&controls_map, &opened_map, &sequences_map, &playback_id);
                    }
                }
                Ok(())
            },
        );
        enqueue_result.map_err(|error| speech_execution_error(TEXT_TO_SPEECH, error))?;

        Ok(json!({
            "playback_id": request.playback_id,
            "segment_index": segment_index,
            "accepted": true,
        }))
    }

    /// Opens a new playback turn on first use for `playback_id`, or appends
    /// the next segment when the turn is already active, returning the
    /// assigned `segment_index`.
    pub(super) fn open_or_append_playback_segment(
        &self,
        request: &TextToSpeechRequest,
    ) -> Result<u64, HostCapabilityError> {
        if workspace_session(&request.session_id) {
            let owners = self
                .playback_owners
                .lock()
                .map_err(|_| failed(TEXT_TO_SPEECH, "playback owners mutex poisoned"))?;
            return Ok(owners
                .get(&request.playback_id)
                .map_or(0, |owner| owner.segment_index + 1));
        }
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        let is_new_turn = frontend
            .voice_playback_status()
            .active_playback_id
            .as_deref()
            != Some(request.playback_id.as_str());
        if is_new_turn {
            frontend
                .open_voice_playback(request.playback_id.clone())
                .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        }
        let snapshot = frontend
            .append_voice_playback_chunk(VoicePlaybackChunk {
                playback_id: request.playback_id.clone(),
                media_type: "audio/pcm".to_string(),
            })
            .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        Ok(snapshot
            .state
            .voice_playback
            .segments
            .back()
            .map(|segment| segment.segment_index)
            .unwrap_or(0))
    }

    pub(super) fn current_playback_segment_index(
        &self,
        playback_id: &str,
    ) -> Result<u64, HostCapabilityError> {
        if let Some(owner) = self.workspace_playback_owner(playback_id) {
            return Ok(owner.segment_index);
        }
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        frontend
            .voice_playback_status()
            .segments
            .iter()
            .rev()
            .find(|segment| segment.playback_id == playback_id)
            .map(|segment| segment.segment_index)
            .ok_or_else(|| unavailable(TEXT_TO_SPEECH, "no playback segment is open to close"))
    }

    pub(super) fn remember_playback_owner(
        &self,
        playback_id: &str,
        plugin_id: &str,
        session_id: &str,
        segment_index: u64,
    ) {
        if let Ok(mut map) = self.playback_owners.lock() {
            let failure = map
                .get(playback_id)
                .filter(|owner| owner.session_id == session_id)
                .and_then(|owner| owner.failure.clone());
            map.insert(
                playback_id.to_string(),
                PlaybackOwner {
                    plugin_id: plugin_id.to_string(),
                    session_id: session_id.to_string(),
                    segment_index,
                    failure,
                },
            );
        }
    }

    pub(super) fn playback_control(&self, playback_id: &str) -> InvocationControl {
        self.playback_controls
            .lock()
            .map(|mut controls| {
                controls
                    .entry(playback_id.to_string())
                    .or_insert_with(|| InvocationControl::with_deadline(Duration::from_secs(600)))
                    .clone()
            })
            .unwrap_or_else(|_| InvocationControl::with_deadline(Duration::from_secs(600)))
    }

    /// Cancels an outstanding stream for `playback_id` (barge-in): stops the
    /// background synthesis worker, evicts owner/control tracking, and
    /// marks every queued segment `Cancelled`.
    pub(super) fn cancel_text_to_speech(
        &self,
        playback_id: &str,
    ) -> Result<Value, HostCapabilityError> {
        let workspace = self.workspace_playback_owner(playback_id).is_some();
        if let Ok(map) = self.playback_controls.lock()
            && let Some(control) = map.get(playback_id)
        {
            control.cancel();
        }
        if let Ok(mut map) = self.playback_owners.lock() {
            map.remove(playback_id);
        }
        cleanup_synthesis(
            &self.playback_controls,
            &self.playback_opened,
            &self.playback_sequences,
            playback_id,
        );
        let _ = self
            .voice_playback_transport
            .publish(VoicePlaybackTransportEvent::Cancelled {
                playback_id: playback_id.to_string(),
            });
        if !workspace {
            let mut frontend = self
                .frontend
                .lock()
                .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
            frontend
                .cancel_voice_playback(playback_id)
                .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        }
        Ok(json!({"playback_id": playback_id, "accepted": true, "cancelled": true}))
    }

    /// Applies an observed playback status and returns its owner and retained synthesis error.
    pub(crate) fn report_voice_playback_status(
        &self,
        playback_id: &str,
        status: VoicePlaybackStatus,
    ) -> Result<Option<PlaybackOwner>, HostCapabilityError> {
        if self.report_audio_playback(playback_id, status) {
            return Ok(None);
        }
        let owner = self
            .playback_owners
            .lock()
            .ok()
            .and_then(|map| map.get(playback_id).cloned());
        let status = if owner.as_ref().is_some_and(|owner| owner.failure.is_some()) {
            VoicePlaybackStatus::Failed
        } else {
            status
        };
        if let Some(owner) = owner
            .as_ref()
            .filter(|owner| workspace_session(&owner.session_id))
        {
            if matches!(
                status,
                VoicePlaybackStatus::Completed
                    | VoicePlaybackStatus::Cancelled
                    | VoicePlaybackStatus::Failed
            ) {
                if let Ok(mut owners) = self.playback_owners.lock() {
                    owners.remove(playback_id);
                }
                cleanup_synthesis(
                    &self.playback_controls,
                    &self.playback_opened,
                    &self.playback_sequences,
                    playback_id,
                );
            }
            return Ok(Some(owner.clone()));
        }
        let mut frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(TEXT_TO_SPEECH, "frontend mutex poisoned"))?;
        let state = frontend.voice_playback_status();
        if state.active_playback_id.as_deref() != Some(playback_id) {
            // The renderer reported a playback turn that is no longer the
            // active one (the turn was replaced by the next response, or the
            // session was replaced). That is normal lifecycle noise: the
            // active turn's own reports drive the session. Failing here used
            // to surface as a bogus session last_error ("expected active
            // voice playback id") and degrade live voice sessions.
            drop(frontend);
            if let Ok(mut map) = self.playback_owners.lock() {
                map.remove(playback_id);
            }
            cleanup_synthesis(
                &self.playback_controls,
                &self.playback_opened,
                &self.playback_sequences,
                playback_id,
            );
            return Ok(None);
        }
        let segment_index = state
            .segments
            .iter()
            .rev()
            .find(|segment| segment.playback_id == playback_id)
            .map(|segment| segment.segment_index)
            .unwrap_or(0);
        frontend
            .set_voice_playback_status(playback_id, status)
            .map_err(|error| failed(TEXT_TO_SPEECH, &error.to_string()))?;
        drop(frontend);
        if matches!(
            status,
            VoicePlaybackStatus::Completed
                | VoicePlaybackStatus::Cancelled
                | VoicePlaybackStatus::Failed
        ) {
            if let Ok(mut map) = self.playback_owners.lock() {
                map.remove(playback_id);
            }
            cleanup_synthesis(
                &self.playback_controls,
                &self.playback_opened,
                &self.playback_sequences,
                playback_id,
            );
        }
        Ok(owner.map(|mut owner| {
            owner.segment_index = segment_index;
            owner
        }))
    }

    fn workspace_playback_owner(&self, playback_id: &str) -> Option<PlaybackOwner> {
        let owners = self.playback_owners.lock().ok()?;
        let owner = owners.get(playback_id)?;
        workspace_session(&owner.session_id).then(|| owner.clone())
    }
}

pub(super) fn record_synthesis_failure(
    owners: &Mutex<HashMap<String, PlaybackOwner>>,
    playback_id: &str,
    control: &InvocationControl,
    message: &str,
) -> bool {
    if control.is_cancelled() {
        return false;
    }
    owners.lock().is_ok_and(|mut owners| {
        owners.get_mut(playback_id).is_some_and(|owner| {
            let failure = format!("Speech generation failed: {message}");
            owner.failure.get_or_insert(failure);
            true
        })
    })
}
