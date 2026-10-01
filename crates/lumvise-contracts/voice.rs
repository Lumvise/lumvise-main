use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantRealtimeMediaMode {
    LocalTranscription,
    DirectVoicePassthrough,
    DirectVoiceAndLiveVideoPassthrough,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSessionPhase {
    Idle,
    Listening,
    Transcribing,
    Thinking,
    Speaking,
    Interrupted,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceSessionRecord {
    pub session_id: String,
    pub assistant_session_id: Option<String>,
    pub phase: VoiceSessionPhase,
    pub active_transcript: Option<String>,
    pub active_provider_run_id: Option<String>,
    pub active_tts_playback_run_id: Option<String>,
    pub cancellation_revision: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceSessionResponse {
    pub session: Option<VoiceSessionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceSessionEventRecord {
    pub id: i64,
    pub voice_session_id: String,
    pub assistant_session_id: Option<String>,
    pub event_type: String,
    pub message: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceSessionEventsResponse {
    pub events: Vec<VoiceSessionEventRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpsertVoiceSessionRequest {
    pub session_id: String,
    pub assistant_session_id: Option<String>,
    pub phase: VoiceSessionPhase,
    pub active_transcript: Option<String>,
    pub active_provider_run_id: Option<String>,
    pub active_tts_playback_run_id: Option<String>,
    pub increment_cancellation_revision: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordVoiceSessionEventRequest {
    pub voice_session_id: String,
    pub assistant_session_id: Option<String>,
    pub event_type: String,
    pub message: String,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTranscriptRevisionKind {
    Partial,
    Final,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceTranscriptRevisionRecord {
    pub id: i64,
    pub request_id: String,
    pub assistant_session_id: Option<String>,
    pub revision: u64,
    pub kind: VoiceTranscriptRevisionKind,
    pub transcript: String,
    pub source: String,
    pub committed_turn_id: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordVoiceTranscriptRevisionRequest {
    pub request_id: String,
    pub assistant_session_id: Option<String>,
    pub revision: Option<u64>,
    pub kind: VoiceTranscriptRevisionKind,
    pub transcript: String,
    pub source: Option<String>,
    pub committed_turn_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceTranscriptRevisionResponse {
    pub revision: VoiceTranscriptRevisionRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceTranscriptRevisionsResponse {
    pub revisions: Vec<VoiceTranscriptRevisionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceBackendStatus {
    pub owner: String,
    pub available: bool,
    pub capture_backend: String,
    pub stt_backend: String,
    pub tts_backend: String,
    pub listen_command_configured: bool,
    pub transcript_override_configured: bool,
    pub stt_model_path: Option<String>,
    pub vad_model_path: Option<String>,
    pub features: Vec<String>,
    pub status_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceTranscriptionRequest {
    pub request_id: String,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceAudioSnippetPurpose {
    Vad,
    Final,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubmitVoiceAudioSnippetRequest {
    pub request_id: String,
    pub revision: Option<u64>,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub purpose: Option<VoiceAudioSnippetPurpose>,
    pub observed_levels: Option<VoiceAudioLevels>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceAudioLevels {
    pub average_rms: f32,
    pub peak: f32,
    pub duration_ms: u64,
    pub noise_floor_rms: f32,
    pub noise_floor_peak: f32,
    pub speech_reference_rms: f32,
    pub speech_reference_peak: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceSuggestedThresholds {
    pub silence_rms_threshold: f32,
    pub silence_peak_threshold: f32,
    pub speech_start_rms_threshold: f32,
    pub meaningful_speech_rms_threshold: f32,
    pub meaningful_speech_peak_threshold: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceTranscriptionResponse {
    pub transcript: String,
    pub speech_detected: bool,
    pub levels: VoiceAudioLevels,
    pub suggested_thresholds: VoiceSuggestedThresholds,
    pub retained_capture_path: Option<String>,
    pub backend: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceAudioSnippetResponse {
    pub event: Option<VoiceSessionEventRecord>,
    pub transcript: Option<String>,
    pub final_transcript: Option<String>,
    pub completed: bool,
    pub transcription: Option<VoiceTranscriptionResponse>,
    #[serde(default)]
    pub media_mode: Option<AssistantRealtimeMediaMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoicePlaybackStatus {
    Queued,
    Playing,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoicePlaybackItemRecord {
    pub playback_id: String,
    pub voice_session_id: String,
    pub assistant_session_id: Option<String>,
    pub text: String,
    pub status: VoicePlaybackStatus,
    pub cancellation_revision: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoicePlaybackQueueResponse {
    pub items: Vec<VoicePlaybackItemRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateVoicePlaybackItemRequest {
    pub playback_id: String,
    pub status: VoicePlaybackStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceSynthesisRequest {
    pub text: String,
    pub voice: Option<String>,
    pub output_device_id: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceSynthesisResponse {
    pub mime_type: String,
    pub audio: Vec<u8>,
    pub backend: String,
    pub message: String,
}
/// A feedback view projected from an Assistant session.
///
/// Feedback routes never create a standalone feedback request record. They
/// expose the latest transcript, response, and playback configuration already
/// owned by the selected Assistant session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackSessionProjectionV2 {
    pub session: crate::AssistantSessionStateV2,
    pub decision: String,
    pub transcript: Option<String>,
    pub response: Option<String>,
    pub playback_enabled: bool,
    pub voice_id: Option<String>,
}

/// Response from a feedback observation route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackSessionResponseV2 {
    pub feedback: FeedbackSessionProjectionV2,
}
