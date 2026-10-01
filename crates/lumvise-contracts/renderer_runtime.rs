use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RendererSettingsResponse {
    pub settings: Value,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateRendererSettingsRequest {
    pub patch: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateMachineProcessRecord {
    pub process: String,
    pub current_state: String,
    pub available_events: Vec<String>,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateMachineOverviewResponse {
    pub processes: Vec<StateMachineProcessRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowDisplayMode {
    Compact,
    Shell,
    Fullscreen,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WhiteboardSurfaceState {
    Minimized,
    Shell,
    Maximized,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransitionWhiteboardSurfaceRequest {
    pub state: WhiteboardSurfaceState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransitionWhiteboardSurfaceResponse {
    pub state: WhiteboardSurfaceState,
    pub requested_finish_confirmation: bool,
    pub voice_request_id: Option<String>,
    #[serde(default)]
    pub browser_actions: Vec<BrowserEngineAction>,
    #[serde(default)]
    pub rust_actions: Vec<RustRuntimeAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserRuntimeState {
    pub expanded: bool,
    pub display_mode: WindowDisplayMode,
    pub whiteboard_surface: WhiteboardSurfaceState,
    pub orb_mode: Option<String>,
    pub voice_phase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncBrowserRuntimeStateRequest {
    pub state: BrowserRuntimeState,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncBrowserRuntimeStateResponse {
    pub accepted_state: BrowserRuntimeState,
    #[serde(default)]
    pub browser_actions: Vec<BrowserEngineAction>,
    #[serde(default)]
    pub rust_actions: Vec<RustRuntimeAction>,
    pub requested_finish_confirmation: bool,
    pub voice_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserEngineAction {
    SetExpanded { expanded: bool },
    SetWindowDisplayMode { mode: WindowDisplayMode },
    SetWhiteboardSurface { state: WhiteboardSurfaceState },
    SetOrbMode { mode: String },
    ActivateRecording { request_id: Option<String> },
    DeactivateRecording,
    PlayVoice { playback_id: Option<String> },
    StopVoicePlayback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RustRuntimeAction {
    EnsureInteractiveAssistantSurface,
    RequestAssistantFinishConfirmation,
    PersistRendererSettings,
    CancelVoicePlayback,
    PauseAssistantSession,
    ResumeAssistantSession,
}
