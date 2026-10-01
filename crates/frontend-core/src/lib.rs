//! Frontend Core owns Lumvise renderer state, widget behavior, and desktop bridge contracts.

mod canvas;
mod context_menu;
#[cfg(feature = "desktop-app")]
mod desktop;
mod error;
mod live_audio;
mod modality;
pub use live_audio::LiveAudioInput;
mod settings;
mod state;
mod types;
mod view_registry;
mod voice;
mod window;
pub use canvas::{
    CanvasElement, CanvasPatch, CanvasRevisionRecord, CanvasSnapshot, MAIN_CANVAS_ID,
};
pub use context_menu::{ContextMenuItem, ContextMenuModel, ContextMenuSection};
#[cfg(feature = "desktop-app")]
pub use desktop::{
    DesktopAppBridgeRequest, DesktopAppBridgeResponse, DesktopAppConfig, DesktopLifecyclePort,
    DesktopPluginViewAsset, DesktopSemanticGraphBridge, DesktopSemanticGraphRequest,
    DesktopSettingsBridge, DesktopSpeechStreamEvent, DesktopSpeechStreamEventSink,
    DesktopVoiceBridge, DesktopVoicePlaybackEvent, DesktopVoicePlaybackEventSink,
    DesktopWhiteboardBridge, PendingDesktopLifecyclePort,
};
pub use error::{FrontendError, Result};
pub use modality::{ModalityStreamCatalog, ModalityStreamState, ModalityStreamsInterface};
pub use settings::{
    AppSettings, AppSettingsPatch, AssistantEngine, AssistantModelOption, AssistantModelSource,
    AssistantProviderCatalog, AssistantProviderOption, AudioDeviceCatalog, AudioDeviceOption,
    ListeningMode,
};
pub use state::{
    DashboardState, FrontendCore, FrontendRuntimeSnapshot, FrontendRuntimeState, FrontendStatus,
    OrbWidgetState, SpawnedAppState, WhiteboardState,
};
pub use types::{
    AppLifecycle, DashboardView, DashboardVisibility, ModalityStreamKind, ModalityStreamPhase,
    OrbMode, OrbVisibility, SurfaceMode, WhiteboardSurface,
};
pub use view_registry::{
    RendererViewDescriptor, RendererViewMenuPlacement, RendererViewSource, RendererViewSurface,
    ViewRegistry,
};
pub use voice::{
    MAX_VOICE_PLAYBACK_SEGMENTS, VoiceAudioChunk, VoicePlaybackChunk, VoicePlaybackSegment,
    VoicePlaybackState, VoicePlaybackStatus, VoiceRecording, VoiceRecordingState,
    VoiceRecordingStatus,
};
pub use window::{
    COMPACT_OFFSET_X, COMPACT_OFFSET_Y, COMPACT_WIDGET_SIZE, HOST_MINIMIZE_COLLAPSE_SCRIPT,
    MINIMIZE_RESTORE_DELAY_MS, SHELL_HEIGHT, SHELL_WIDTH, WidgetBounds, WindowDisplayState,
    WindowLayout, WindowManagementCommand, WindowManagementPlan, WorkArea, clamp_widget_bounds,
    compact_bounds_for_collapse, logical_i32_to_physical, logical_to_physical,
    settings_window_bounds, widget_bounds_for_mode, window_management_plan_for_mode,
    work_area_for_bounds, work_area_for_point,
};
