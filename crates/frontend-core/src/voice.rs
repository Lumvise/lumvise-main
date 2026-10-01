use std::collections::VecDeque;

use crate::error::{FrontendError, Result};
use crate::state::{FrontendCore, FrontendRuntimeSnapshot};
use crate::types::{DashboardView, ModalityStreamKind, ModalityStreamPhase, OrbMode};
use serde::{Deserialize, Serialize};

/// Upper bound on queued playback segments per turn — a producer that keeps
/// appending without the renderer ever draining status is an error case, not
/// unbounded growth (mirrors the transport-level backpressure in
/// `subscribe_voice_playback`).
pub const MAX_VOICE_PLAYBACK_SEGMENTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceRecordingStatus {
    Idle,
    Recording,
    Completed,
    Failed,
}

/// Lifecycle of one queued playback segment. Ownership of *when* speech
/// plays moved to App Core (see `crates/app-core/src/plugin/plugin_host_capabilities.rs`);
/// this state only ever mirrors that owner's real, reported progress —
/// `Playing`/`Completed`/`Cancelled` are set exclusively via
/// `FrontendCore::set_voice_playback_status`, driven by the renderer's own
/// PCM sink, never guessed locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoicePlaybackStatus {
    Queued,
    Playing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceRecording {
    pub recording_id: String,
    pub media_type: String,
    pub audio: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceRecordingState {
    pub status: VoiceRecordingStatus,
    pub active_recording_id: Option<String>,
    pub media_type: Option<String>,
    pub audio_bytes: usize,
    pub current_recording: Option<VoiceRecording>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceAudioChunk {
    pub recording_id: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub final_chunk: bool,
}

/// One ordered segment of a playback turn. Audio bytes never live here —
/// they stream straight from App Core's synthesis producer down the
/// `subscribe_voice_playback` Tauri channel to the renderer's PCM sink; this
/// is metadata only, for status observation (`observe_assistant_state` and
/// friends).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePlaybackSegment {
    pub playback_id: String,
    pub segment_index: u64,
    pub media_type: String,
    pub status: VoicePlaybackStatus,
}

/// One playback chunk being appended to the active turn. Mirrors
/// `VoiceAudioChunk`'s shape on the recording side; unlike that struct this
/// carries no bytes — the segment's `segment_index` is assigned by
/// `FrontendCore::append_voice_playback_chunk` and returned in the resulting
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePlaybackChunk {
    pub playback_id: String,
    pub media_type: String,
}

/// Bounded, ordered queue of playback segments for the active turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePlaybackState {
    pub active_playback_id: Option<String>,
    pub segments: VecDeque<VoicePlaybackSegment>,
    pub last_error: Option<String>,
}

impl Default for VoiceRecordingState {
    fn default() -> Self {
        Self {
            status: VoiceRecordingStatus::Idle,
            active_recording_id: None,
            media_type: None,
            audio_bytes: 0,
            current_recording: None,
            last_error: None,
        }
    }
}

impl Default for VoicePlaybackState {
    fn default() -> Self {
        Self {
            active_playback_id: None,
            segments: VecDeque::new(),
            last_error: None,
        }
    }
}

impl FrontendCore {
    /// Returns the current voice recording endpoint state.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert_eq!(core.voice_recording_status().status, lumvise_frontend_core::VoiceRecordingStatus::Idle);
    /// ```
    pub fn voice_recording_status(&self) -> VoiceRecordingState {
        self.state.voice_recording.clone()
    }

    /// Returns the current buffered voice recording, when one exists.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert!(core.current_voice_recording().is_none());
    /// ```
    pub fn current_voice_recording(&self) -> Option<VoiceRecording> {
        self.state.voice_recording.current_recording.clone()
    }

    /// Starts a centralized voice recording session.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// assert_eq!(core.trigger_voice_recording("rec-1", "audio/wav").unwrap().state.voice_recording.audio_bytes, 0);
    /// ```
    pub fn trigger_voice_recording(
        &mut self,
        recording_id: impl Into<String>,
        media_type: impl Into<String>,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.require_view_enabled(&DashboardView::VoiceRecording)?;
        let recording = new_voice_recording(recording_id.into(), media_type.into())?;
        self.state.voice_recording = VoiceRecordingState::started(recording);
        self.apply_voice_dashboard(DashboardView::VoiceRecording);
        self.state.streams = self.state.streams.clone().with_stream(
            ModalityStreamKind::Speech,
            ModalityStreamPhase::Capturing,
            None,
        )?;
        self.snapshot()
    }

    /// Streams an audio chunk into the active voice recording.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.trigger_voice_recording("rec-1", "audio/wav").unwrap();
    /// let chunk = lumvise_frontend_core::VoiceAudioChunk {
    ///     recording_id: "rec-1".into(),
    ///     media_type: "audio/wav".into(),
    ///     bytes: vec![1, 2, 3],
    ///     final_chunk: true,
    /// };
    /// assert_eq!(core.stream_voice_audio(chunk).unwrap().state.voice_recording.audio_bytes, 3);
    /// ```
    pub fn stream_voice_audio(
        &mut self,
        chunk: VoiceAudioChunk,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.state.voice_recording.append_chunk(chunk)?;
        self.sync_speech_phase_from_recording()?;
        self.snapshot()
    }

    /// Returns the current voice playback endpoint state.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert!(core.voice_playback_status().active_playback_id.is_none());
    /// ```
    pub fn voice_playback_status(&self) -> VoicePlaybackState {
        self.state.voice_playback.clone()
    }

    /// Returns the most recently appended playback segment of the active
    /// turn, when one exists.
    ///
    /// # Example
    ///
    /// ```
    /// let core = lumvise_frontend_core::FrontendCore::default();
    /// assert!(core.current_voice_playback_segment().is_none());
    /// ```
    pub fn current_voice_playback_segment(&self) -> Option<VoicePlaybackSegment> {
        self.state.voice_playback.segments.back().cloned()
    }

    /// Opens a new playback turn, replacing whatever the previous turn
    /// queued. Bytes are never buffered here — they stream straight down
    /// `subscribe_voice_playback` as App Core's synthesis producer emits
    /// them.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// let snapshot = core.open_voice_playback("play-1").unwrap();
    /// assert_eq!(snapshot.state.voice_playback.active_playback_id.as_deref(), Some("play-1"));
    /// ```
    pub fn open_voice_playback(
        &mut self,
        playback_id: impl Into<String>,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        let playback_id = playback_id.into();
        require_non_empty(&playback_id, "non-empty playback id")?;
        self.state.voice_playback = VoicePlaybackState::opened(playback_id);
        self.apply_voice_dashboard(DashboardView::VoicePlayback);
        self.state.orb.mode = OrbMode::Session;
        self.snapshot()
    }

    /// Appends one segment of metadata to the active playback turn. The
    /// assigned `segment_index` is readable from the returned snapshot
    /// (`segments.back()`).
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.open_voice_playback("play-1").unwrap();
    /// let chunk = lumvise_frontend_core::VoicePlaybackChunk {
    ///     playback_id: "play-1".into(),
    ///     media_type: "audio/pcm;rate=24000;format=s16le".into(),
    /// };
    /// let snapshot = core.append_voice_playback_chunk(chunk).unwrap();
    /// assert_eq!(snapshot.state.voice_playback.segments.back().unwrap().segment_index, 0);
    /// ```
    pub fn append_voice_playback_chunk(
        &mut self,
        chunk: VoicePlaybackChunk,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.state.voice_playback.append_chunk(chunk)?;
        self.snapshot()
    }

    /// Signals that no further segments will be appended to this playback
    /// turn. Segment status keeps advancing afterward as the renderer
    /// reports real progress via `set_voice_playback_status`.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.open_voice_playback("play-1").unwrap();
    /// assert!(core.close_voice_playback("play-1").is_ok());
    /// ```
    pub fn close_voice_playback(
        &mut self,
        playback_id: impl Into<String>,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.state.voice_playback.close(playback_id.into())?;
        self.snapshot()
    }

    /// Cancels the active playback turn (e.g. on barge-in): every queued or
    /// playing segment becomes `Cancelled` in one step.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.open_voice_playback("play-1").unwrap();
    /// let snapshot = core.cancel_voice_playback("play-1").unwrap();
    /// assert_eq!(snapshot.state.orb.mode, lumvise_frontend_core::OrbMode::Idle);
    /// ```
    pub fn cancel_voice_playback(
        &mut self,
        playback_id: impl Into<String>,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.set_voice_playback_status(playback_id, VoicePlaybackStatus::Cancelled)
    }

    /// Updates the status of every segment in the active playback turn.
    ///
    /// # Example
    ///
    /// ```
    /// let mut core = lumvise_frontend_core::FrontendCore::default();
    /// core.spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1440, 900, 1.0)).unwrap();
    /// core.open_voice_playback("play-1").unwrap();
    /// let chunk = lumvise_frontend_core::VoicePlaybackChunk {
    ///     playback_id: "play-1".into(),
    ///     media_type: "audio/pcm;rate=24000;format=s16le".into(),
    /// };
    /// core.append_voice_playback_chunk(chunk).unwrap();
    /// assert_eq!(core.set_voice_playback_status("play-1", lumvise_frontend_core::VoicePlaybackStatus::Playing).unwrap().state.orb.mode, lumvise_frontend_core::OrbMode::Activity);
    /// ```
    pub fn set_voice_playback_status(
        &mut self,
        playback_id: impl Into<String>,
        status: VoicePlaybackStatus,
    ) -> Result<FrontendRuntimeSnapshot> {
        self.require_spawned()?;
        self.state
            .voice_playback
            .set_status(playback_id.into(), status)?;
        self.state.orb.mode = orb_mode_for_playback(status);
        self.snapshot()
    }

    fn apply_voice_dashboard(&mut self, view: DashboardView) {
        self.apply_whiteboard_surface(crate::types::WhiteboardSurface::Shell);
        self.state.dashboard.active_view = view.clone();
        self.state.whiteboard.active_view = view;
    }

    fn sync_speech_phase_from_recording(&mut self) -> Result<()> {
        let phase = match self.state.voice_recording.status {
            VoiceRecordingStatus::Recording => ModalityStreamPhase::Capturing,
            VoiceRecordingStatus::Completed => ModalityStreamPhase::Ready,
            VoiceRecordingStatus::Failed => ModalityStreamPhase::Failed,
            VoiceRecordingStatus::Idle => ModalityStreamPhase::Ready,
        };
        let error = self.state.voice_recording.last_error.clone();
        self.state.streams =
            self.state
                .streams
                .clone()
                .with_stream(ModalityStreamKind::Speech, phase, error)?;
        Ok(())
    }
}

impl VoiceRecordingState {
    fn started(recording: VoiceRecording) -> Self {
        Self {
            status: VoiceRecordingStatus::Recording,
            active_recording_id: Some(recording.recording_id.clone()),
            media_type: Some(recording.media_type.clone()),
            audio_bytes: 0,
            current_recording: Some(recording),
            last_error: None,
        }
    }

    fn append_chunk(&mut self, chunk: VoiceAudioChunk) -> Result<()> {
        self.validate_chunk(&chunk)?;
        let Some(recording) = self.current_recording.as_mut() else {
            return Err(FrontendError::invalid_value(
                chunk.recording_id,
                "active voice recording",
            ));
        };
        recording.audio.extend(chunk.bytes);
        self.audio_bytes = recording.audio.len();
        self.status = recording_status_for_chunk(chunk.final_chunk);
        Ok(())
    }

    fn validate_chunk(&self, chunk: &VoiceAudioChunk) -> Result<()> {
        require_non_empty(&chunk.recording_id, "non-empty recording id")?;
        require_non_empty(&chunk.media_type, "non-empty audio media type")?;
        require_non_empty_bytes(&chunk.bytes, "non-empty audio chunk")?;
        self.require_matching_recording(chunk)?;
        Ok(())
    }

    fn require_matching_recording(&self, chunk: &VoiceAudioChunk) -> Result<()> {
        if self.status != VoiceRecordingStatus::Recording {
            return Err(FrontendError::invalid_value(
                "idle",
                "active voice recording",
            ));
        }
        if self.active_recording_id.as_deref() != Some(chunk.recording_id.as_str()) {
            return Err(FrontendError::invalid_value(
                chunk.recording_id.clone(),
                "active voice recording id",
            ));
        }
        if self.media_type.as_deref() != Some(chunk.media_type.as_str()) {
            return Err(FrontendError::invalid_value(
                chunk.media_type.clone(),
                "active recording media type",
            ));
        }
        Ok(())
    }
}

impl VoicePlaybackState {
    fn opened(playback_id: String) -> Self {
        Self {
            active_playback_id: Some(playback_id),
            segments: VecDeque::new(),
            last_error: None,
        }
    }

    fn append_chunk(&mut self, chunk: VoicePlaybackChunk) -> Result<()> {
        require_non_empty(&chunk.playback_id, "non-empty playback id")?;
        require_non_empty(&chunk.media_type, "non-empty audio media type")?;
        if self.active_playback_id.as_deref() != Some(chunk.playback_id.as_str()) {
            return Err(FrontendError::invalid_value(
                chunk.playback_id,
                "active voice playback id",
            ));
        }
        if self.segments.len() >= MAX_VOICE_PLAYBACK_SEGMENTS {
            self.segments.pop_front();
        }
        let segment_index = self
            .segments
            .back()
            .map(|segment| segment.segment_index + 1)
            .unwrap_or(0);
        self.segments.push_back(VoicePlaybackSegment {
            playback_id: chunk.playback_id,
            segment_index,
            media_type: chunk.media_type,
            status: VoicePlaybackStatus::Queued,
        });
        Ok(())
    }

    fn close(&mut self, playback_id: String) -> Result<()> {
        require_non_empty(&playback_id, "non-empty playback id")?;
        if self.active_playback_id.as_deref() != Some(playback_id.as_str()) {
            return Err(FrontendError::invalid_value(
                playback_id,
                "active voice playback id",
            ));
        }
        Ok(())
    }

    fn set_status(&mut self, playback_id: String, status: VoicePlaybackStatus) -> Result<()> {
        require_non_empty(&playback_id, "non-empty playback id")?;
        if self.active_playback_id.as_deref() != Some(playback_id.as_str()) {
            return Err(FrontendError::invalid_value(
                playback_id,
                "active voice playback id",
            ));
        }
        for segment in self
            .segments
            .iter_mut()
            .filter(|segment| segment.playback_id == playback_id)
        {
            segment.status = status;
        }
        if status == VoicePlaybackStatus::Failed {
            self.last_error = Some("voice playback failed".to_string());
        }
        Ok(())
    }
}

fn new_voice_recording(recording_id: String, media_type: String) -> Result<VoiceRecording> {
    require_non_empty(&recording_id, "non-empty recording id")?;
    require_non_empty(&media_type, "non-empty audio media type")?;
    Ok(VoiceRecording {
        recording_id,
        media_type,
        audio: Vec::new(),
    })
}

fn require_non_empty(value: &str, expected: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(FrontendError::invalid_value(value, expected));
    }
    Ok(())
}

fn require_non_empty_bytes(bytes: &[u8], expected: &str) -> Result<()> {
    if bytes.is_empty() {
        return Err(FrontendError::invalid_value("0 bytes", expected));
    }
    Ok(())
}

fn recording_status_for_chunk(final_chunk: bool) -> VoiceRecordingStatus {
    if final_chunk {
        return VoiceRecordingStatus::Completed;
    }
    VoiceRecordingStatus::Recording
}

fn orb_mode_for_playback(status: VoicePlaybackStatus) -> OrbMode {
    match status {
        VoicePlaybackStatus::Queued => OrbMode::Session,
        VoicePlaybackStatus::Playing => OrbMode::Activity,
        VoicePlaybackStatus::Completed | VoicePlaybackStatus::Cancelled => OrbMode::Idle,
        VoicePlaybackStatus::Failed => OrbMode::Error,
    }
}
