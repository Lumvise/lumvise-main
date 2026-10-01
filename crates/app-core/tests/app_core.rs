use lumvise_app_core::{AppCore, CanvasElement, CanvasPatch};
use lumvise_db_core::{
    ArtifactTextVector, ChangeHookScope, LocalPersistence, RelationalPersistence,
    SemanticPersistence,
};
use lumvise_frontend_core::{
    AppSettingsPatch, DashboardView, VoiceAudioChunk, VoicePlaybackChunk, VoicePlaybackStatus,
    VoiceRecordingStatus, WorkArea,
};
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_neural_core::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use lumvise_neural_core::llm_providers::{
    LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities, LlmRequest, LlmResponse,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use serde_json::json;
use std::sync::Arc;

fn app_with_registry(registry: LlmProviderRegistry) -> AppCore {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    AppCore::new(
        semantic,
        relational,
        lumvise_frontend_core::FrontendCore::default(),
        registry,
    )
}

struct NamedFakeProvider;

impl LlmProvider for NamedFakeProvider {
    fn provider_id(&self) -> &str {
        "local"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: "local".into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, _request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        Ok(LlmResponse {
            provider_id: "local".into(),
            model: "local-model".into(),
            content: "local response".into(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
}

#[test]
fn app_core_coordinates_frontend_modalities_and_canvas() {
    let app = AppCore::in_memory().unwrap();
    app.frontend().spawn_app(default_work_area()).unwrap();

    let started = app
        .modalities()
        .start_voice_recording("rec-1", "audio/wav")
        .unwrap();
    app.modalities()
        .stream_voice_audio(voice_chunk("rec-1", true))
        .unwrap();
    let canvas = app.frontend().update_canvas(canvas_patch()).unwrap();

    let recording = app.modalities().current_voice_recording().unwrap().unwrap();
    assert_eq!(
        started.state.dashboard.active_view,
        DashboardView::VoiceRecording
    );
    assert_eq!(recording.audio, vec![82, 73, 70, 70]);
    assert_eq!(
        app.modalities().voice_recording_state().unwrap().status,
        VoiceRecordingStatus::Completed
    );
    assert_eq!(canvas.revision, 1);
    assert_eq!(app.frontend().canvas().unwrap().elements.len(), 1);
}

#[test]
fn modality_endpoints_cover_recording_playback_and_screenshots() {
    let app = AppCore::in_memory().unwrap();
    app.frontend().spawn_app(default_work_area()).unwrap();

    app.modalities()
        .start_voice_recording("rec-e2e", "audio/wav")
        .unwrap();
    app.modalities()
        .stream_voice_audio(voice_chunk("rec-e2e", true))
        .unwrap();
    app.modalities().open_voice_playback("play-e2e").unwrap();
    app.modalities()
        .append_voice_playback_chunk(playback_chunk("play-e2e"))
        .unwrap();
    app.modalities()
        .set_voice_playback_status("play-e2e", VoicePlaybackStatus::Playing)
        .unwrap();
    let screenshot = app
        .modalities()
        .record_screenshot("shot-e2e", "image/png", screenshot_png())
        .unwrap();
    let completed = app
        .modalities()
        .set_voice_playback_status("play-e2e", VoicePlaybackStatus::Completed)
        .unwrap();

    let recording = app.modalities().current_voice_recording().unwrap().unwrap();
    let playback = app
        .modalities()
        .current_voice_playback_segment()
        .unwrap()
        .unwrap();
    assert_eq!(recording.audio, vec![82, 73, 70, 70]);
    assert_eq!(playback.playback_id, "play-e2e");
    assert_eq!(
        app.modalities().latest_screenshot().unwrap(),
        Some(screenshot)
    );
    assert_eq!(
        completed
            .state
            .voice_playback
            .segments
            .back()
            .unwrap()
            .status,
        VoicePlaybackStatus::Completed
    );
}

#[test]
fn modality_records_reject_empty_media_and_retrieve_latest_payloads() {
    let app = AppCore::in_memory().unwrap();
    let modalities = app.modalities();

    let bad_screenshot = modalities
        .record_screenshot("shot-empty", "image/png", Vec::new())
        .unwrap_err();
    let bad_screen_frame = modalities
        .record_screen_frame_broadcast("frame-empty", "image/png", Vec::new())
        .unwrap_err();
    let screenshot = modalities
        .record_screenshot("shot-1", "image/png", vec![137, 80])
        .unwrap();
    let broadcast = modalities
        .record_desktop_broadcast("broadcast-1", "window.changed", json!({"title": "Main"}))
        .unwrap();

    assert!(bad_screenshot.to_string().contains("non-empty media bytes"));
    assert!(
        bad_screen_frame
            .to_string()
            .contains("non-empty media bytes")
    );
    assert_eq!(modalities.latest_screenshot().unwrap(), Some(screenshot));
    assert_eq!(
        modalities.latest_desktop_broadcast().unwrap(),
        Some(broadcast)
    );
}

#[test]
fn modality_records_keep_screen_frame_broadcasts_separate() {
    let app = AppCore::in_memory().unwrap();
    let modalities = app.modalities();

    let frame = modalities
        .record_screen_frame_broadcast("frame-1", "image/png", vec![137, 80])
        .unwrap();
    let second_frame = modalities
        .record_screen_frame_broadcast("frame-2", "image/png", vec![137, 81])
        .unwrap();

    assert_eq!(frame.frame_id, "frame-1");
    assert_eq!(frame.media_type, "image/png");
    assert_eq!(
        modalities.latest_screen_frame_broadcast().unwrap(),
        Some(second_frame.clone())
    );
    assert_eq!(
        modalities.recent_screen_frame_broadcasts(1).unwrap(),
        vec![second_frame]
    );
    assert!(
        modalities
            .recent_screen_frame_broadcasts(0)
            .unwrap()
            .is_empty()
    );
    assert!(modalities.latest_screenshot().unwrap().is_none());
}

#[test]
fn frontend_settings_persist_through_sql_database() {
    let app = AppCore::in_memory().unwrap();
    let frontend = app.frontend();

    frontend
        .apply_app_settings_patch(&AppSettingsPatch::VoiceRecordingEnabled(false))
        .unwrap();
    let stored = frontend
        .apply_app_settings_patch(&AppSettingsPatch::DesktopBroadcastsEnabled(false))
        .unwrap();
    let loaded = frontend.app_settings().unwrap();

    assert_eq!(stored.scope, "frontend");
    assert!(!loaded.voice_recording_enabled);
    assert!(!loaded.desktop_broadcasts_enabled);
    let persisted = app
        .database()
        .setting("frontend", "app_settings")
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(persisted["voice_recording_enabled"], json!(false));
    assert_eq!(persisted["desktop_broadcasts_enabled"], json!(false));
}

#[test]
fn database_endpoint_exposes_settings_and_blobs() {
    let app = AppCore::in_memory().unwrap();
    let database = app.database();

    let setting = database
        .set_setting("app", "theme", &json!("dark"))
        .unwrap();
    database
        .put_blob("blob://artifact/root", "root-note", "text/plain", b"body")
        .unwrap();

    assert_eq!(setting.value, json!("dark"));
    assert_eq!(
        database
            .blob("blob://artifact/root")
            .unwrap()
            .unwrap()
            .content,
        b"body"
    );
    assert!(
        database
            .changes_since_revision(
                ChangeHookScope {
                    project_root: "/repo".into(),
                    entity_kinds: Default::default(),
                },
                0,
                100,
            )
            .unwrap()
            .changed
            .is_empty()
    );
}

#[test]
fn llm_endpoint_routes_to_configured_neural_provider() {
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(NamedFakeProvider)]).unwrap();
    let app = app_with_registry(registry);

    let response = app.llms().complete("local", &llm_request()).unwrap();

    assert_eq!(response.content, "local response");
    assert_eq!(response.provider_id, "local");
}

#[test]
fn llm_endpoint_exposes_provider_capabilities() {
    let registry =
        LlmProviderRegistry::from_provider_instances(vec![Box::new(NamedFakeProvider)]).unwrap();
    let app = app_with_registry(registry);

    let capabilities = app.llms().capabilities("local").unwrap();

    assert_eq!(capabilities.provider_id, "local");
    assert_eq!(
        capabilities.streamed_text_output,
        LlmCapabilitySupport::Supported
    );
}

#[test]
fn llm_endpoint_reports_unknown_provider() {
    let app = AppCore::in_memory().unwrap();
    let error = app.llms().complete("missing", &llm_request()).unwrap_err();

    assert!(error.to_string().contains("configured provider id"));
}

fn default_work_area() -> WorkArea {
    WorkArea::new(0, 0, 1440, 900, 1.0)
}

fn voice_chunk(recording_id: &str, final_chunk: bool) -> VoiceAudioChunk {
    VoiceAudioChunk {
        recording_id: recording_id.to_string(),
        media_type: "audio/wav".to_string(),
        bytes: vec![82, 73, 70, 70],
        final_chunk,
    }
}

fn playback_chunk(playback_id: &str) -> VoicePlaybackChunk {
    VoicePlaybackChunk {
        playback_id: playback_id.to_string(),
        media_type: "audio/pcm;rate=24000;format=s16le".to_string(),
    }
}

fn screenshot_png() -> Vec<u8> {
    vec![137, 80, 78, 71, 13, 10, 26, 10]
}

fn canvas_patch() -> CanvasPatch {
    CanvasPatch {
        canvas_id: "main".to_string(),
        elements: vec![CanvasElement {
            element_id: "node-1".to_string(),
            element_kind: "note".to_string(),
            content: json!({"text": "hello"}),
        }],
    }
}

fn llm_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream: false,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

struct FreshVectorizer;

impl lumvise_db_core::ArtifactTextVectorizer for FreshVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> lumvise_db_core::Result<ArtifactTextVector> {
        Ok(ArtifactTextVector {
            engine_id: "fresh-engine".into(),
            model: Some("fresh-model".into()),
            dimensions: 2,
            vector: vec![text.len() as f32, 1.0],
            normalized: false,
            metadata: json!({}),
        })
    }
}

#[test]
fn fresh_database_production_persistence_reopens() {
    let temp = tempfile::tempdir().expect("fresh app database directory");
    let path = temp.path().join("app.sqlite");
    let persistence =
        Arc::new(LocalPersistence::open(&path).expect("open fresh local persistence"));
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    let app = AppCore::new(
        semantic,
        relational,
        lumvise_frontend_core::FrontendCore::default(),
        LlmProviderRegistry::empty(),
    );
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(FreshVectorizer))
        .expect("install fresh vectorizer");
    let broker = app.host_capability_broker();
    let request = |plugin_id: &str, capability_id: &str, input| HostCapabilityRequest {
        plugin_id: plugin_id.into(),
        invocation_id: "fresh-invocation".into(),
        call_id: "fresh-call".into(),
        capability_id: capability_id.into(),
        required_version: "1".into(),
        input,
    };

    broker
        .invoke(request(
            "compiled.fresh",
            "storage.plugin",
            json!({"operation": "ensure_table", "table_name": "notes", "schema": {}}),
        ))
        .expect("compiled-plugin table");
    broker
        .invoke(request(
            "compiled.fresh",
            "storage.plugin",
            json!({"operation": "put_row", "table_name": "notes", "row_key": "first", "value": {"ok": true}}),
        ))
        .expect("compiled-plugin row");

    let element = json!({
        "project_root": "/fresh",
        "semantic_element_id": "fresh-element",
        "semantic_source_id": "source",
        "path": "src/fresh.rs",
        "element_kind": "function",
        "name": "fresh_handler",
        "parent_element_id": null,
        "content_fingerprint": null,
        "start_line": 1,
        "end_line": 2,
        "lifecycle": "active",
        "metadata": {}
    });
    broker
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({"operation": "sync_structure", "project_root": "/fresh", "elements": [element], "relationships": []}),
        ))
        .expect("semantic graph write");
    broker
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({
                "operation": "store_element_name_vectors",
                "project_root": "/fresh",
                "vectors": [{
                    "semantic_element_id": "fresh-element",
                    "project_root": "/fresh",
                    "source_text": "fresh_handler",
                    "vector": {
                        "engine_id": "fresh-engine",
                        "model": "fresh-model",
                        "dimensions": 2,
                        "vector": [1.0, 0.0],
                        "normalized": true,
                        "metadata": {}
                    }
                }]
            }),
        ))
        .expect("semantic vector write");

    let snapshot = broker
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({"operation": "project_snapshot", "scope": {"project_root": "/fresh"}, "artifact_namespace": null}),
        ))
        .expect("semantic snapshot read");
    assert_eq!(snapshot["project_root"], "/fresh");
    assert_eq!(
        snapshot["elements"][0]["semantic_element_id"],
        "fresh-element"
    );
    assert!(
        snapshot["commit_version"]
            .as_i64()
            .is_some_and(|revision| revision > 0)
    );
    let vectors = broker
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({"operation": "search_element_name_vectors", "project_root": "/fresh", "query": [1.0, 0.0], "k": 1, "engine_id": "fresh-engine", "model": "fresh-model"}),
        ))
        .expect("semantic vector lookup");
    assert_eq!(vectors["results"][0]["id"], "fresh-element");

    let hook_cycle = app
        .plugin_endpoints()
        .run_change_hook_cycle(0)
        .expect("change hook delivery cycle");
    assert!(hook_cycle.hooks.iter().any(|hook| hook.acknowledged));
    let changes = app
        .database()
        .changes_since_revision(
            ChangeHookScope {
                project_root: "/fresh".into(),
                entity_kinds: Default::default(),
            },
            0,
            10,
        )
        .expect("change hook revision read");
    assert!(!changes.changed.is_empty());
    drop(broker);
    drop(app);

    let persistence =
        Arc::new(LocalPersistence::open(&path).expect("reopen fresh local persistence"));
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    let reopened = AppCore::new(
        semantic,
        relational,
        lumvise_frontend_core::FrontendCore::default(),
        LlmProviderRegistry::empty(),
    );
    let reopened_broker = reopened.host_capability_broker();
    let row = reopened_broker
        .invoke(request(
            "compiled.fresh",
            "storage.plugin",
            json!({"operation": "get_row", "table_name": "notes", "row_key": "first"}),
        ))
        .expect("reopened compiled-plugin row");
    assert_eq!(row["row"]["value"], json!({"ok": true}));
    let reopened_snapshot = reopened_broker
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({"operation": "project_snapshot", "scope": {"project_root": "/fresh"}, "artifact_namespace": null}),
        ))
        .expect("reopened semantic snapshot");
    assert_eq!(
        reopened_snapshot["elements"][0]["semantic_element_id"],
        "fresh-element"
    );
    let reopened_vectors = reopened_broker
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({"operation": "search_element_name_vectors", "project_root": "/fresh", "query": [1.0, 0.0], "k": 1, "engine_id": "fresh-engine", "model": "fresh-model"}),
        ))
        .expect("reopened semantic vector lookup");
    assert_eq!(reopened_vectors["results"][0]["id"], "fresh-element");
}
