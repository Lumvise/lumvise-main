//! Desktop launch configuration and pure default bridges; native launch stays outside this crate.
use super::bridge::{
    DesktopSemanticGraphBridge, DesktopSettingsBridge, DesktopSpeechStreamEvent,
    DesktopSpeechStreamEventSink, DesktopVoiceBridge, DesktopVoicePlaybackEventSink,
    DesktopWhiteboardBridge,
};
use super::lifecycle::PendingDesktopLifecyclePort;
use crate::{AppSettings, AppSettingsPatch, WorkArea};
use std::sync::{Arc, Mutex, MutexGuard};

/// Configures a desktop launch without depending on a native platform implementation.
///
/// # Example
/// ```
/// let config = lumvise_frontend_core::DesktopAppConfig::default();
/// assert_eq!(config.title, "Lumvise");
/// ```
#[derive(Clone, Debug)]
pub struct DesktopAppConfig {
    pub work_area: Option<WorkArea>,
    pub settings: AppSettings,
    pub title: String,
    pub renderer_tool: Option<String>,
    pub show_in_taskbar: bool,
    pub settings_bridge: Arc<dyn DesktopSettingsBridge>,
    pub semantic_graph_bridge: Arc<dyn DesktopSemanticGraphBridge>,
    pub voice_bridge: Arc<dyn DesktopVoiceBridge>,
    pub whiteboard_bridge: Arc<dyn DesktopWhiteboardBridge>,
    pub lifecycle_port: Arc<PendingDesktopLifecyclePort>,
}

impl Default for DesktopAppConfig {
    fn default() -> Self {
        Self {
            work_area: None,
            settings: AppSettings::default(),
            title: "Lumvise".to_string(),
            renderer_tool: None,
            show_in_taskbar: true,
            settings_bridge: Arc::new(DemoSettingsBridge::default()),
            semantic_graph_bridge: Arc::new(DemoSemanticGraphBridge),
            voice_bridge: Arc::new(DemoVoiceBridge),
            whiteboard_bridge: Arc::new(DemoWhiteboardBridge),
            lifecycle_port: Arc::new(PendingDesktopLifecyclePort::default()),
        }
    }
}
impl PartialEq for DesktopAppConfig {
    fn eq(&self, other: &Self) -> bool {
        self.work_area == other.work_area
            && self.settings == other.settings
            && self.title == other.title
            && self.renderer_tool == other.renderer_tool
            && self.show_in_taskbar == other.show_in_taskbar
    }
}

#[derive(Debug, Default)]
pub(super) struct DemoSettingsBridge {
    settings: Mutex<AppSettings>,
}

impl DemoSettingsBridge {
    fn app_settings_guard(&self) -> Result<MutexGuard<'_, AppSettings>, String> {
        self.settings.lock().map_err(|error| {
            format!(
                "demo app settings mutex was poisoned: {error}; expected an available settings lock"
            )
        })
    }
}

#[derive(Debug)]
pub(super) struct DemoSemanticGraphBridge;

#[derive(Debug)]
pub(super) struct DemoVoiceBridge;

#[derive(Debug)]
pub(super) struct DemoWhiteboardBridge;

impl DesktopSettingsBridge for DemoSettingsBridge {
    fn app_settings_snapshot(&self) -> Result<AppSettings, String> {
        Ok(self.app_settings_guard()?.clone())
    }

    fn bulb_visible(&self) -> Result<bool, String> {
        Ok(self.app_settings_snapshot()?.bulb_visible)
    }

    fn apply_app_settings_patch(&self, patch: &AppSettingsPatch) -> Result<(), String> {
        self.app_settings_guard()?.apply_app_settings_patch(patch);
        Ok(())
    }
}

impl DesktopSemanticGraphBridge for DemoSemanticGraphBridge {}

impl DesktopVoiceBridge for DemoVoiceBridge {
    fn submit_voice_snippet(
        &self,
        _snippet: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "transcript": null,
            "finalTranscript": null,
            "completed": false,
            "transcription": { "backend": "desktop-demo", "speechDetected": false }
        }))
    }

    fn transcribe_audio(
        &self,
        _request_id: String,
        _audio: Vec<f32>,
        _sample_rate: u32,
        _options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "transcript": "",
            "speechDetected": false,
            "levels": null,
            "suggestedThresholds": null,
            "retainedCapturePath": null
        }))
    }

    fn synthesize_speech(
        &self,
        _request_id: String,
        _text: String,
        _voice: Option<String>,
        _options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({ "audio": [], "mimeType": "audio/wav" }))
    }

    fn stream_synthesize_speech(
        &self,
        _request_id: String,
        _text: String,
        _voice: Option<String>,
        on_event: &mut DesktopSpeechStreamEventSink<'_>,
    ) -> Result<(), String> {
        on_event(DesktopSpeechStreamEvent::Complete)
    }

    fn subscribe_voice_playback(
        &self,
        _on_event: &mut DesktopVoicePlaybackEventSink<'_>,
    ) -> Result<(), String> {
        // Demo bridge has no producer; block until the app tears the
        // subscription down, matching the real bridge's "install once"
        // shape without ever emitting an event.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    fn set_voice_playback_status(
        &self,
        _playback_id: String,
        _status: String,
    ) -> Result<(), String> {
        Ok(())
    }
}

impl DesktopWhiteboardBridge for DemoWhiteboardBridge {
    fn sync_user_canvas_scene(
        &self,
        session_id: String,
        _canvas_id: Option<&str>,
        scene_json: String,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "sessionId": session_id,
            "sceneBytes": scene_json.len(),
            "synced": false,
            "backend": "desktop-demo"
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, AppSettingsPatch, DemoSettingsBridge, DesktopSettingsBridge};

    #[test]
    fn desktop_config_preserves_launch_defaults_and_default_bridges() {
        let config = super::DesktopAppConfig::default();
        assert!(config.show_in_taskbar);
        assert_eq!(config.title, "Lumvise");
        assert_eq!(config, super::DesktopAppConfig::default());
        config
            .settings_bridge
            .apply_app_settings_patch(&crate::AppSettingsPatch::BulbVisible(false))
            .unwrap();
        assert!(
            config
                .voice_bridge
                .synthesize_speech("demo".into(), "hello".into(), None, None)
                .is_ok()
        );
    }

    #[test]
    fn desktop_config_demo_settings_snapshot_retains_patches() {
        let config = super::DesktopAppConfig::default();
        let bridge = config.settings_bridge;
        let initial = bridge.app_settings_snapshot().unwrap();
        assert_eq!(initial, config.settings);
        for patch in [
            AppSettingsPatch::BulbVisible(false),
            AppSettingsPatch::SpeechSynthesisEnabled(false),
        ] {
            bridge.apply_app_settings_patch(&patch).unwrap();
        }
        let snapshot = bridge.app_settings_snapshot().unwrap();
        assert!(!snapshot.bulb_visible);
        assert!(snapshot.speech_recognition_enabled);
        assert!(!snapshot.speech_synthesis_enabled);
        assert!(!bridge.bulb_visible().unwrap());
    }

    #[test]
    fn desktop_config_demo_settings_keeps_instances_and_snapshots_independent() {
        let bridge = DemoSettingsBridge::default();
        let mut snapshot = bridge.app_settings_snapshot().unwrap();
        snapshot.bulb_visible = false;
        assert!(bridge.app_settings_snapshot().unwrap().bulb_visible);
        bridge
            .apply_app_settings_patch(&AppSettingsPatch::BulbVisible(false))
            .unwrap();
        let separate = super::DesktopAppConfig::default().settings_bridge;
        assert_eq!(
            separate.app_settings_snapshot().unwrap(),
            AppSettings::default()
        );
    }

    #[test]
    fn desktop_config_demo_settings_rejects_poisoned_snapshot_and_patch() {
        let bridge = DemoSettingsBridge::default();
        let _ = std::panic::catch_unwind(|| {
            let _settings = bridge.settings.lock().unwrap();
            panic!("poison demo settings for regression coverage");
        });
        let snapshot_error = bridge.app_settings_snapshot().unwrap_err();
        let patch_error = bridge
            .apply_app_settings_patch(&AppSettingsPatch::BulbVisible(false))
            .unwrap_err();
        assert!(snapshot_error.contains("poisoned"));
        assert!(snapshot_error.contains("expected an available settings lock"));
        assert_eq!(snapshot_error, patch_error);
    }
}
