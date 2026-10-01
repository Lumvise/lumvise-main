//! Public desktop contracts; platform implementation belongs to desktop-shell.
mod app;
mod bridge;
mod lifecycle;
pub use app::DesktopAppConfig;
pub use bridge::{
    DesktopAppBridgeRequest, DesktopAppBridgeResponse, DesktopPluginViewAsset,
    DesktopSemanticGraphBridge, DesktopSemanticGraphRequest, DesktopSettingsBridge,
    DesktopSpeechStreamEvent, DesktopSpeechStreamEventSink, DesktopVoiceBridge,
    DesktopVoicePlaybackEvent, DesktopVoicePlaybackEventSink, DesktopWhiteboardBridge,
};
pub use lifecycle::{DesktopLifecyclePort, PendingDesktopLifecyclePort};
