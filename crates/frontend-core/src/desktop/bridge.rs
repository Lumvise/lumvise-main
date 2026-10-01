use crate::{AppSettingsPatch, AssistantProviderCatalog, WorkArea};
use lumvise_db_core::{CompactSemanticGraphProjection, SemanticGraphGranularity};

pub trait DesktopSettingsBridge: std::fmt::Debug + Send + Sync {
    /// Reads the saved launcher preference after startup, e.g. `bridge.bulb_visible()`.
    fn bulb_visible(&self) -> Result<bool, String> {
        Ok(true)
    }

    fn apply_app_settings_patch(&self, patch: &AppSettingsPatch) -> Result<(), String>;
    fn set_provider_api_key(
        &self,
        provider_id: String,
        api_key: String,
    ) -> Result<serde_json::Value, String> {
        let _ = (provider_id, api_key);
        Err("provider API key bridge is not configured".to_string())
    }
    fn clear_provider_api_key(&self, provider_id: String) -> Result<serde_json::Value, String> {
        let _ = provider_id;
        Err("provider API key clear bridge is not configured".to_string())
    }
    fn set_provider_endpoint(
        &self,
        provider_id: String,
        endpoint: String,
    ) -> Result<serde_json::Value, String> {
        let _ = (provider_id, endpoint);
        Err("provider endpoint bridge is not configured".to_string())
    }
    fn clear_provider_endpoint(&self, provider_id: String) -> Result<serde_json::Value, String> {
        let _ = provider_id;
        Err("provider endpoint clear bridge is not configured".to_string())
    }
    fn frontend_spawned(&self, _work_area: WorkArea) -> Result<(), String> {
        Ok(())
    }
    fn drain_frontend_actions(&self) -> Result<Vec<serde_json::Value>, String> {
        Ok(Vec::new())
    }
    /// Delivers only actions owned by this native window, e.g. `lumvise-settings`.
    fn drain_window_actions(&self, window_label: &str) -> Result<Vec<serde_json::Value>, String> {
        if window_label == "lumvise-frontend" {
            self.drain_frontend_actions()
        } else {
            Ok(Vec::new())
        }
    }
    /// Returns the active-source assistant provider catalog (TOML-backed).
    fn assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog, String> {
        Ok(AssistantProviderCatalog::default())
    }
    /// Re-runs provider discovery/probing and returns the refreshed catalog.
    /// Unlike [`Self::assistant_provider_catalog`], which only reads the last
    /// synchronized snapshot, this forces a new sync so the user can recover
    /// from a stale catalog (CLI login completed, network restored, operator
    /// edited the model catalog file) without touching a credential.
    fn refresh_assistant_provider_catalog(&self) -> Result<AssistantProviderCatalog, String> {
        Err("assistant provider catalog refresh bridge is not configured".to_string())
    }
    /// Returns the currently persisted assistant engine value and model id.
    fn assistant_selection(&self) -> Result<(String, Option<String>), String> {
        Err("assistant selection bridge is not configured".to_string())
    }
    /// Lists ready compiled renderer Views and their asset-server base URL.
    fn compiled_plugin_views(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"baseUrl": null, "views": []}))
    }
    /// Reads signed bytes of a ready native View, e.g. `read_plugin_view_asset(id, view, "")`.
    fn read_plugin_view_asset(
        &self,
        plugin_id: &str,
        view_id: &str,
        relative_path: &str,
    ) -> Result<DesktopPluginViewAsset, String> {
        Err(format!(
            "View {plugin_id}/{view_id}/{relative_path}: expected a configured signed asset bridge"
        ))
    }
    /// Executes one signed View Host API through the host policy broker.
    fn invoke_compiled_view_host_api(
        &self,
        plugin_id: String,
        view_id: String,
        api_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let _ = (plugin_id, view_id, api_id, input);
        Err("compiled View Host API bridge is not configured".into())
    }
    /// Returns the managed-model catalog, hardware facts, and per-model
    /// lifecycle status as one renderer-safe JSON snapshot.
    fn managed_model_snapshot(&self) -> Result<serde_json::Value, String> {
        Err("managed model bridge is not configured".to_string())
    }
    /// Selects one managed model: activates it immediately if already
    /// downloaded, or starts/joins its download otherwise.
    fn select_managed_model(&self, model_id: String) -> Result<serde_json::Value, String> {
        let _ = model_id;
        Err("managed model bridge is not configured".to_string())
    }
    /// Retries a failed managed-model selection.
    fn retry_managed_model(&self, model_id: String) -> Result<serde_json::Value, String> {
        let _ = model_id;
        Err("managed model bridge is not configured".to_string())
    }
    /// Returns configured/not-configured status for every LLM Provider that
    /// accepts a local API key. Never returns the stored secret value.
    fn provider_credential_status(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}

/// Typed request for the app-owned indexed semantic graph projection.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSemanticGraphRequest {
    pub provider_id: Option<String>,
    pub project_root: Option<String>,
    pub target_path: Option<String>,
    pub granularity: SemanticGraphGranularity,
    #[serde(default = "default_recursive")]
    pub recursive: bool,
    #[serde(default)]
    pub include_external: bool,
    #[serde(default)]
    pub include_first_neighbors: bool,
}

fn default_recursive() -> bool {
    true
}

/// One app-bridge HTTP request issued by the workspace UI.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DesktopAppBridgeRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// The router's status code and body, returned verbatim.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DesktopAppBridgeResponse {
    pub status: u16,
    pub body: String,
}

/// Verified native plugin asset; the package owns its bytes and browser policy.
#[derive(Debug, Clone)]
pub struct DesktopPluginViewAsset {
    pub path: String,
    pub bytes: Vec<u8>,
    pub content_security_policy: String,
}

/// Native seam for the canonical indexed semantic graph projection.
pub trait DesktopSemanticGraphBridge: std::fmt::Debug + Send + Sync {
    /// Waits up to 15 seconds for a storage revision wake; an equal result is a
    /// timeout and a lower result means the caller must reset its cursor.
    /// Example: `bridge.wait_for_storage_revision(last_applied_revision)`.
    fn wait_for_storage_revision(&self, after_revision: i64) -> Result<i64, String> {
        Err(format!(
            "storage revision {after_revision}: expected a configured storage revision bridge"
        ))
    }

    fn list_indexed_semantic_graph_roots(&self) -> Result<Vec<String>, String> {
        Err("indexed semantic graph bridge is not configured".to_string())
    }

    fn project_indexed_semantic_graph(
        &self,
        request: DesktopSemanticGraphRequest,
    ) -> Result<CompactSemanticGraphProjection, String> {
        let _ = request;
        Err("indexed semantic graph bridge is not configured".to_string())
    }

    /// Reads media without UTF-8 or JSON conversion, restricted to an indexed project.
    /// Example: `bridge.read_source_file_bytes(root, "movie.mp4".into())`.
    fn read_source_file_bytes(
        &self,
        project_root: String,
        path: String,
    ) -> Result<Vec<u8>, String> {
        Err(format!(
            "Source {project_root}/{path}: binary source bridge is not configured"
        ))
    }

    /// Reads one stored canvas image file by its `canvas-file:` content ref.
    /// Example: `bridge.read_canvas_file("canvas-file:c1:abc…".into())`.
    fn read_canvas_file(&self, content_ref: String) -> Result<Vec<u8>, String> {
        let _ = &content_ref;
        Err(format!(
            "Canvas file {content_ref}: canvas file bridge is not configured"
        ))
    }

    /// Stores one canvas image file and returns its camelCase `CanvasFileRef`
    /// JSON (`contentRef`, `mimeType`, `byteSize`).
    fn write_canvas_file(
        &self,
        artifact_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        let _ = (artifact_id, media_type, bytes);
        Err("canvas file bridge is not configured".to_string())
    }

    /// Routes one app-bridge HTTP request in-process for the workspace UI.
    /// Non-2xx responses come back as data so the renderer can show the
    /// backend's own message.
    fn app_bridge_request(
        &self,
        request: DesktopAppBridgeRequest,
    ) -> Result<DesktopAppBridgeResponse, String> {
        let _ = request;
        Err("app bridge is not configured".to_string())
    }
}

pub trait DesktopVoiceBridge: std::fmt::Debug + Send + Sync {
    fn send_live_audio(&self, _input: crate::LiveAudioInput) -> Result<(), String> {
        Err("Direct audio requires an available desktop audio connection".into())
    }
    fn invoke_plugin_capability(
        &self,
        plugin_id: String,
        capability_id: String,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let _ = (plugin_id, capability_id, input);
        Err("plugin capability bridge is not configured".to_string())
    }

    fn submit_voice_snippet(&self, snippet: serde_json::Value)
    -> Result<serde_json::Value, String>;
    fn transcribe_audio(
        &self,
        request_id: String,
        audio: Vec<f32>,
        sample_rate: u32,
        options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String>;
    fn synthesize_speech(
        &self,
        request_id: String,
        text: String,
        voice: Option<String>,
        options: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String>;
    fn stream_synthesize_speech(
        &self,
        request_id: String,
        text: String,
        voice: Option<String>,
        on_event: &mut DesktopSpeechStreamEventSink<'_>,
    ) -> Result<(), String>;

    /// Blocks forwarding real-time playback events until the app tears the
    /// subscription down. Installed once at bridge startup (not per
    /// request, unlike `stream_synthesize_speech`) — App Core is the sole
    /// producer, pushing `Opened`/`AudioChunk`/`Closed`/`Cancelled` as
    /// `modalities.text_to_speech` streams a session-owned turn.
    fn subscribe_voice_playback(
        &self,
        on_event: &mut DesktopVoicePlaybackEventSink<'_>,
    ) -> Result<(), String>;

    /// Reports a real playback status transition observed by the
    /// renderer's own PCM sink, so App Core can advance whichever session
    /// owns `playback_id` (`Speaking`/`AwaitingUser`/`Listening`, W2).
    fn set_voice_playback_status(&self, playback_id: String, status: String) -> Result<(), String>;

    fn record_screen_frame_broadcast(
        &self,
        frame_id: String,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        let _ = (frame_id, media_type, bytes);
        Err("screen frame broadcast bridge is not configured".to_string())
    }
}
/// One ordered native speech-synthesis event delivered to the renderer.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DesktopSpeechStreamEvent {
    AudioChunk {
        sequence: u64,
        audio: Vec<u8>,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Complete,
    Error {
        message: String,
    },
}

/// Callback accepted by the desktop voice bridge while native audio is produced.
pub type DesktopSpeechStreamEventSink<'a> =
    dyn FnMut(DesktopSpeechStreamEvent) -> Result<(), String> + Send + 'a;

/// One ordered playback-transport event, pushed by App Core's synthesis
/// producer as `modalities.text_to_speech` streams a session-owned turn.
/// `Opened` fires once per turn before its first `AudioChunk`; `Closed`
/// fires once the turn's last segment finishes streaming; `Cancelled`
/// fires on barge-in or a hard synthesis failure.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DesktopVoicePlaybackEvent {
    Opened {
        playback_id: String,
        #[serde(rename = "mimeType")]
        media_type: String,
        #[serde(rename = "sampleRateHz", skip_serializing_if = "Option::is_none")]
        sample_rate_hz: Option<u32>,
    },
    AudioChunk {
        playback_id: String,
        sequence: u64,
        audio: Vec<u8>,
    },
    Closed {
        playback_id: String,
    },
    Cancelled {
        playback_id: String,
    },
}

/// Callback accepted by the desktop voice bridge while playback events are
/// produced. Unlike [`DesktopSpeechStreamEventSink`] this callback outlives
/// a single request — it stays installed for the process lifetime of the
/// one `subscribe_voice_playback` command.
pub type DesktopVoicePlaybackEventSink<'a> =
    dyn FnMut(DesktopVoicePlaybackEvent) -> Result<(), String> + Send + 'a;

pub trait DesktopWhiteboardBridge: std::fmt::Debug + Send + Sync {
    /// Syncs a user-edited canvas scene. `canvas_id` defaults to
    /// [`crate::MAIN_CANVAS_ID`] when the renderer does not supply one.
    fn sync_user_canvas_scene(
        &self,
        session_id: String,
        canvas_id: Option<&str>,
        scene_json: String,
    ) -> Result<serde_json::Value, String>;
}
