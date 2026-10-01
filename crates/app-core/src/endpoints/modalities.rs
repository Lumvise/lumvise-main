use super::frontend::require_non_empty;
use crate::{AppCore, AppCoreError, Result};
use chrono::Utc;
use lumvise_frontend_core::{
    AppSettings, FrontendRuntimeSnapshot, ModalityStreamKind, ModalityStreamPhase, VoiceAudioChunk,
    VoicePlaybackChunk, VoicePlaybackSegment, VoicePlaybackState, VoicePlaybackStatus,
    VoiceRecording, VoiceRecordingState,
};
use lumvise_neural_core::voice2text::{Voice2TextRequest, Voice2TextResponse};
use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotRecord {
    pub screenshot_id: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopBroadcastRecord {
    pub broadcast_id: String,
    pub event_kind: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenFrameBroadcastRecord {
    pub frame_id: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceTranscriptionRecord {
    pub response: Voice2TextResponse,
    pub input_device_id: Option<String>,
}

pub struct ModalityEndpoints<'app> {
    app: &'app AppCore,
}

impl<'app> ModalityEndpoints<'app> {
    pub(crate) fn new(app: &'app AppCore) -> Self {
        Self { app }
    }

    /// Starts a centralized voice recording session.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// app.modalities().start_voice_recording("rec", "audio/wav").unwrap();
    /// ```
    pub fn start_voice_recording(
        &self,
        recording_id: &str,
        media_type: &str,
    ) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.trigger_voice_recording(recording_id, media_type)?)
    }

    /// Streams voice audio into the active frontend recording buffer.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// app.modalities().start_voice_recording("rec", "audio/wav").unwrap();
    /// let chunk = lumvise_frontend_core::VoiceAudioChunk { recording_id: "rec".into(), media_type: "audio/wav".into(), bytes: vec![1], final_chunk: true };
    /// app.modalities().stream_voice_audio(chunk).unwrap();
    /// ```
    pub fn stream_voice_audio(&self, chunk: VoiceAudioChunk) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.stream_voice_audio(chunk)?)
    }

    /// Retrieves the current voice recording state.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let _state = app.modalities().voice_recording_state().unwrap();
    /// ```
    pub fn voice_recording_state(&self) -> Result<VoiceRecordingState> {
        Ok(self.lock_frontend()?.voice_recording_status())
    }

    /// Retrieves the current buffered voice recording.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.modalities().current_voice_recording().unwrap().is_none());
    /// ```
    pub fn current_voice_recording(&self) -> Result<Option<VoiceRecording>> {
        Ok(self.lock_frontend()?.current_voice_recording())
    }

    /// Transcribes the current frontend voice recording through App Core.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let transcript = app.modalities()
    ///     .transcribe_current_recording(None)
    ///     .unwrap();
    /// assert!(!transcript.response.transcript.is_empty());
    /// ```
    pub fn transcribe_current_recording(
        &self,
        model: Option<String>,
    ) -> Result<VoiceTranscriptionRecord> {
        let (recording, settings) = self.recording_and_settings()?;
        let request = Voice2TextRequest {
            audio: recording.audio,
            media_type: recording.media_type,
            model,
        };
        let response = self.transcribe_recording(&request)?;
        Ok(VoiceTranscriptionRecord {
            response,
            input_device_id: settings.input_device_id,
        })
    }

    /// Opens a new playback turn, replacing whatever the previous turn
    /// queued.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// app.modalities().open_voice_playback("p").unwrap();
    /// ```
    pub fn open_voice_playback(&self, playback_id: &str) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.open_voice_playback(playback_id)?)
    }

    /// Appends one segment of playback metadata to the active turn.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// app.modalities().open_voice_playback("p").unwrap();
    /// let chunk = lumvise_frontend_core::VoicePlaybackChunk { playback_id: "p".into(), media_type: "audio/pcm;rate=24000;format=s16le".into() };
    /// app.modalities().append_voice_playback_chunk(chunk).unwrap();
    /// ```
    pub fn append_voice_playback_chunk(
        &self,
        chunk: VoicePlaybackChunk,
    ) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.append_voice_playback_chunk(chunk)?)
    }

    /// Signals that no further segments will be appended to the active
    /// playback turn.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// app.modalities().open_voice_playback("p").unwrap();
    /// app.modalities().close_voice_playback("p").unwrap();
    /// ```
    pub fn close_voice_playback(&self, playback_id: &str) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.close_voice_playback(playback_id)?)
    }

    /// Updates the active voice playback status.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let _state = app.modalities().voice_playback_state().unwrap();
    /// ```
    pub fn set_voice_playback_status(
        &self,
        playback_id: &str,
        status: VoicePlaybackStatus,
    ) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.set_voice_playback_status(playback_id, status)?)
    }

    /// Retrieves the current voice playback state.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let _state = app.modalities().voice_playback_state().unwrap();
    /// ```
    pub fn voice_playback_state(&self) -> Result<VoicePlaybackState> {
        Ok(self.lock_frontend()?.voice_playback_status())
    }

    /// Retrieves the most recently appended playback segment of the active
    /// turn.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.modalities().current_voice_playback_segment().unwrap().is_none());
    /// ```
    pub fn current_voice_playback_segment(&self) -> Result<Option<VoicePlaybackSegment>> {
        Ok(self.lock_frontend()?.current_voice_playback_segment())
    }

    /// Updates a modality stream state such as screenshots or desktop broadcasts.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// let area = lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0);
    /// app.frontend().spawn_app(area).unwrap();
    /// app.modalities().set_modality_stream(lumvise_frontend_core::ModalityStreamKind::Screenshots, lumvise_frontend_core::ModalityStreamPhase::Ready, None).unwrap();
    /// ```
    pub fn set_modality_stream(
        &self,
        kind: ModalityStreamKind,
        phase: ModalityStreamPhase,
        error: Option<String>,
    ) -> Result<FrontendRuntimeSnapshot> {
        let mut frontend = self.lock_frontend()?;
        Ok(frontend.set_modality_stream(kind, phase, error)?)
    }

    /// Records a screenshot for plugin/API retrieval.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.modalities().record_screenshot("s", "image/png", vec![1]).unwrap();
    /// ```
    pub fn record_screenshot(
        &self,
        screenshot_id: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<ScreenshotRecord> {
        validate_binary_media(screenshot_id, media_type, &bytes)?;
        let record = screenshot_record(screenshot_id, media_type, bytes);
        self.lock_screenshots()?.push(record.clone());
        Ok(record)
    }

    /// Retrieves the newest screenshot record.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.modalities().latest_screenshot().unwrap().is_none());
    /// ```
    pub fn latest_screenshot(&self) -> Result<Option<ScreenshotRecord>> {
        Ok(self.lock_screenshots()?.last().cloned())
    }

    /// Records a desktop broadcast payload for plugin/API retrieval.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.modalities().record_desktop_broadcast("b", "window.changed", serde_json::json!({})).unwrap();
    /// ```
    pub fn record_desktop_broadcast(
        &self,
        broadcast_id: &str,
        event_kind: &str,
        payload: Value,
    ) -> Result<DesktopBroadcastRecord> {
        require_non_empty(broadcast_id, "non-empty broadcast id")?;
        require_non_empty(event_kind, "non-empty broadcast event kind")?;
        let record = desktop_broadcast_record(broadcast_id, event_kind, payload);
        self.lock_desktop_broadcasts()?.push(record.clone());
        Ok(record)
    }

    /// Retrieves the newest desktop broadcast payload.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.modalities().latest_desktop_broadcast().unwrap().is_none());
    /// ```
    pub fn latest_desktop_broadcast(&self) -> Result<Option<DesktopBroadcastRecord>> {
        Ok(self.lock_desktop_broadcasts()?.last().cloned())
    }

    /// Records a live screen frame for model context broadcasting.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.modalities().record_screen_frame_broadcast("f", "image/png", vec![1]).unwrap();
    /// ```
    pub fn record_screen_frame_broadcast(
        &self,
        frame_id: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<ScreenFrameBroadcastRecord> {
        validate_binary_media(frame_id, media_type, &bytes)?;
        let record = screen_frame_broadcast_record(frame_id, media_type, bytes);
        self.lock_screen_frame_broadcasts()?.push(record.clone());
        Ok(record)
    }

    /// Retrieves the newest live screen frame broadcast.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.modalities().latest_screen_frame_broadcast().unwrap().is_none());
    /// ```
    pub fn latest_screen_frame_broadcast(&self) -> Result<Option<ScreenFrameBroadcastRecord>> {
        Ok(self.lock_screen_frame_broadcasts()?.last().cloned())
    }

    /// Retrieves newest live screen frame broadcasts, oldest first.
    ///
    /// # Example
    ///
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.modalities().record_screen_frame_broadcast("f", "image/png", vec![1]).unwrap();
    /// let frames = app.modalities().recent_screen_frame_broadcasts(2).unwrap();
    /// assert_eq!(frames.len(), 1);
    /// ```
    pub fn recent_screen_frame_broadcasts(
        &self,
        limit: usize,
    ) -> Result<Vec<ScreenFrameBroadcastRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let frames = self.lock_screen_frame_broadcasts()?;
        let start = frames.len().saturating_sub(limit);
        Ok(frames[start..].to_vec())
    }

    fn lock_frontend(
        &self,
    ) -> Result<std::sync::MutexGuard<'app, lumvise_frontend_core::FrontendCore>> {
        self.app
            .frontend
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("frontend"))
    }

    fn recording_and_settings(&self) -> Result<(VoiceRecording, AppSettings)> {
        let frontend = self.lock_frontend()?;
        let settings = frontend.app_settings().clone();
        let recording = frontend
            .current_voice_recording()
            .ok_or_else(|| AppCoreError::missing_value("voice_recording", "completed recording"))?;
        Ok((recording, settings))
    }

    fn transcribe_recording(&self, request: &Voice2TextRequest) -> Result<Voice2TextResponse> {
        let service = self
            .app
            .voice2text_service()
            .ok_or_else(|| AppCoreError::unsupported("voice2text", "configured STT service"))?;
        Ok(service.transcribe(request, &InvocationControl::sixty_seconds())?)
    }

    fn lock_screenshots(&self) -> Result<std::sync::MutexGuard<'app, Vec<ScreenshotRecord>>> {
        self.app
            .screenshots
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("screenshots"))
    }

    fn lock_desktop_broadcasts(
        &self,
    ) -> Result<std::sync::MutexGuard<'app, Vec<DesktopBroadcastRecord>>> {
        self.app
            .desktop_broadcasts
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("desktop_broadcasts"))
    }

    fn lock_screen_frame_broadcasts(
        &self,
    ) -> Result<std::sync::MutexGuard<'app, Vec<ScreenFrameBroadcastRecord>>> {
        self.app
            .screen_frame_broadcasts
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("screen_frame_broadcasts"))
    }
}

fn validate_binary_media(id: &str, media_type: &str, bytes: &[u8]) -> Result<()> {
    require_non_empty(id, "non-empty media record id")?;
    require_non_empty(media_type, "non-empty media type")?;
    if bytes.is_empty() {
        return Err(AppCoreError::invalid_value(
            "0 bytes",
            "non-empty media bytes",
        ));
    }
    Ok(())
}

fn screenshot_record(id: &str, media_type: &str, bytes: Vec<u8>) -> ScreenshotRecord {
    ScreenshotRecord {
        screenshot_id: id.to_string(),
        media_type: media_type.to_string(),
        bytes,
        captured_at: Utc::now().to_rfc3339(),
    }
}

fn desktop_broadcast_record(id: &str, event_kind: &str, payload: Value) -> DesktopBroadcastRecord {
    DesktopBroadcastRecord {
        broadcast_id: id.to_string(),
        event_kind: event_kind.to_string(),
        payload,
        created_at: Utc::now().to_rfc3339(),
    }
}

fn screen_frame_broadcast_record(
    id: &str,
    media_type: &str,
    bytes: Vec<u8>,
) -> ScreenFrameBroadcastRecord {
    ScreenFrameBroadcastRecord {
        frame_id: id.to_string(),
        media_type: media_type.to_string(),
        bytes,
        captured_at: Utc::now().to_rfc3339(),
    }
}
