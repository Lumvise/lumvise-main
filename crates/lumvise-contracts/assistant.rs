use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Assistant phase emitted by the compiled Assistant plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantPhaseV2 {
    Idle,
    Countdown,
    Listening,
    Transcribing,
    Thinking,
    Responding,
    Speaking,
    AwaitingUser,
    AwaitingSpawner,
    ConfirmingFinish,
    Completed,
    Paused,
    ProviderTurnFailed,
    Interrupted,
    Failed,
}

impl AssistantPhaseV2 {
    /// Whether this phase ends the session. Example: `AssistantPhaseV2::Completed.is_terminal()`.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Interrupted | Self::Failed)
    }

    /// Stable wire spelling. Example: `AssistantPhaseV2::Speaking.token() == "speaking"`.
    pub fn token(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Countdown => "countdown",
            Self::Listening => "listening",
            Self::Transcribing => "transcribing",
            Self::Thinking => "thinking",
            Self::Responding => "responding",
            Self::Speaking => "speaking",
            Self::AwaitingUser => "awaiting_user",
            Self::AwaitingSpawner => "awaiting_spawner",
            Self::ConfirmingFinish => "confirming_finish",
            Self::Completed => "completed",
            Self::Paused => "paused",
            Self::ProviderTurnFailed => "provider_turn_failed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

/// Knowledge scope accepted by a compiled Assistant session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssistantKnowledgeScopeV2 {
    SessionContext,
    AllProjects,
}

/// How a session delivers its answer: spoken segments plus canvas narration, or
/// a final text reply in Markdown. Missing persisted values load as `Voice`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssistantPresentationV2 {
    #[default]
    Voice,
    Text,
}

/// The terminal cause retained by a completed Assistant session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantTerminalReasonV2 {
    pub code: String,
    pub replaced_by_session_id: Option<String>,
}

/// Typed provider-turn failure retained so a recovered session can render and
/// retry without parsing an error message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantProviderTurnFailureV2 {
    pub tier: String,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Caller-supplied provider result waiting for normal provider completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantPendingNativeProviderResponseV2 {
    pub provider_id: String,
    pub model: String,
    pub content: String,
    #[serde(default)]
    pub segment_count: u64,
    #[serde(default)]
    pub transcript: Option<String>,
}

/// A question awaiting a spawning model's response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantSpawnerQuestionV2 {
    pub request_id: String,
    pub question: String,
    pub context: Option<String>,
    pub status: String,
}

/// The latest response supplied by a spawning model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantSpawnerResponseV2 {
    pub request_id: String,
    pub question: String,
    pub response: String,
}

/// Persisted state owned by the compiled Assistant plugin.
///
/// This is deliberately the plugin state, rather than an app-owned session
/// record. `session_id` is the only session identity exposed by the v2 routes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantSessionStateV2 {
    pub session_id: String,
    pub session_epoch: u64,
    pub mode: String,
    pub phase: AssistantPhaseV2,
    pub prompt: String,
    pub knowledge_scope: AssistantKnowledgeScopeV2,
    #[serde(default)]
    pub presentation: AssistantPresentationV2,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub live_transcription: bool,
    pub voice_id: Option<String>,
    pub countdown_digit: u8,
    pub last_transcript: Option<String>,
    pub last_response: Option<String>,
    /// The current app-engine turn finalized its spoken response through MCP.
    #[serde(default)]
    pub app_response_finalized: bool,
    /// Native audio uses the same Assistant lifecycle and scoped playback identity.
    #[serde(default)]
    pub direct_audio: bool,
    #[serde(default)]
    pub audio_playback_id: Option<String>,
    pub final_summary: Option<String>,
    pub provider_job_id: Option<String>,
    pub pending_native_provider_response: Option<AssistantPendingNativeProviderResponseV2>,
    pub llm_session_id: Option<String>,
    pub paused_phase: Option<AssistantPhaseV2>,
    pub pending_spawner_question: Option<AssistantSpawnerQuestionV2>,
    pub last_spawner_response: Option<AssistantSpawnerResponseV2>,
    pub last_canvas_revision: Option<u64>,
    pub assistant_requested_finish: bool,
    pub last_error: Option<String>,
    pub terminal_reason: Option<AssistantTerminalReasonV2>,
    pub turn_sequence: u64,
    pub turn_id: Option<String>,
    pub provider_attempt: u8,
    pub last_turn_failure: Option<AssistantProviderTurnFailureV2>,
    /// Segment at which app-owned audio playback was interrupted.
    #[serde(default)]
    pub interrupted_at_segment: Option<u64>,
    /// Declared native-LLM caller; `None` for CLI-spawned sessions.
    #[serde(default)]
    pub native_llm_caller: Option<NativeLlmCallerDeclarationV2>,
    /// Unix-ms last-contact liveness timestamp for handoff sessions.
    #[serde(default)]
    pub native_last_contact_at: Option<i64>,
    /// Unix-ms timestamp of the last delivered user turn; user-inactivity reaper clock.
    #[serde(default)]
    pub last_user_turn_at: Option<i64>,
    /// Speech captured while the engine was still thinking (#94); rides the next provider turn.
    #[serde(default)]
    pub queued_user_transcripts: std::collections::VecDeque<String>,
}

/// Caller-supplied identity of the native LLM driving a handoff session.
///
/// Open, caller-supplied engine identity (e.g. "claude", "codex", "gemini");
/// not a closed enum so future providers need no contract change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeLlmCallerDeclarationV2 {
    /// Caller-supplied engine identity, validated non-blank by the plugin.
    pub engine: String,
    /// Caller's own opaque session/thread identifier, logs/metrics only.
    pub instance_id: Option<String>,
}

/// Direct-response state, which has no persisted Assistant session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantDirectStateV2 {
    pub mode: String,
    pub phase: AssistantPhaseV2,
}

/// The stable fields in an LLM completion; provider metadata remains
/// provider-owned and is therefore an extension value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantResponseV2 {
    pub provider_id: String,
    pub model: String,
    pub content: String,
    pub metadata: Value,
}

/// Playback result returned by the configured text-to-speech capability.
///
/// Capability providers may add result fields; the plugin preserves those
/// fields verbatim rather than inventing a voice persistence record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantPlaybackV2 {
    pub playback_id: String,
    #[serde(flatten)]
    pub extension: std::collections::BTreeMap<String, Value>,
}

/// Input accepted by `POST /api/assistant/session` and the feedback-start
/// projection route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartAssistantSessionRequestV2 {
    pub prompt: String,
    pub session_id: Option<String>,
    pub knowledge_scope: Option<AssistantKnowledgeScopeV2>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub live_transcription: Option<bool>,
    pub voice_id: Option<String>,
    pub countdown_digit: Option<u8>,
    pub response_text: Option<String>,
    pub background_request: Option<bool>,
    pub replace: Option<bool>,
    /// Declares a native-LLM caller; only accepted over the MCP start route.
    #[serde(default)]
    pub native_llm_caller: Option<NativeLlmCallerDeclarationV2>,
}
/// Input accepted by the explicit Assistant-session finish route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinishAssistantSessionRequestV2 {
    pub session_id: String,
    pub summary: Option<String>,
    pub status: Option<String>,
}

/// Input accepted by feedback cancellation. Feedback has no separate record:
/// this identifies the Assistant session to finish.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelFeedbackSessionRequestV2 {
    pub session_id: String,
    pub summary: Option<String>,
}

/// Response from a direct Assistant completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantDirectResponseV2 {
    pub state: AssistantDirectStateV2,
    pub transcript: String,
    pub response: AssistantResponseV2,
    pub playback: Option<AssistantPlaybackV2>,
}

/// Response returned when a session starts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantSessionStartResponseV2 {
    pub state: AssistantSessionStateV2,
    pub decision: String,
    pub background_request: Value,
    pub frontend: Option<Value>,
    pub degraded: bool,
    pub session_instructions: String,
    pub active_tools: Vec<String>,
    pub voice_constraints: String,
    pub error: Option<String>,
}

/// Response returned by session observation and finish.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantSessionStateResponseV2 {
    pub state: AssistantSessionStateV2,
    pub decision: String,
    pub response: Option<AssistantResponseV2>,
}

/// Response returned by an explicit session finish.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantSessionFinishResponseV2 {
    pub state: AssistantSessionStateV2,
    pub decision: String,
    pub degraded: bool,
}
