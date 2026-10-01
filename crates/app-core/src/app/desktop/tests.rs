use super::*;
use lumvise_db_core::{RelationalOperation, RelationalResult};
use lumvise_frontend_core::AssistantModelSource;
use lumvise_neural_core::{
    LlmModelDescriptor, LlmModelSource, LlmProviderAvailability, LlmProviderCatalog,
    LlmProviderKind, LlmProviderStatus, ProviderModelInventory, ProviderModelSources,
};
use lumvise_resource_routing::InvocationControl;

#[test]
fn assistant_catalog_bridge_exposes_only_active_source_and_availability() {
    let catalog = LlmProviderCatalog {
        providers: vec![LlmProviderStatus {
            provider_id: "codex".to_string(),
            kind: LlmProviderKind::Codex,
            state: LlmProviderAvailability::InvocationFailed {
                message: "probe failed".to_string(),
            },
            model_sources: ProviderModelSources {
                api: Some(ProviderModelInventory {
                    models: vec![LlmModelDescriptor {
                        id: "api-only".to_string(),
                        display_name: "API Only".to_string(),
                    }],
                    default_model: Some("api-only".to_string()),
                }),
                client: Some(ProviderModelInventory {
                    models: vec![LlmModelDescriptor {
                        id: "client-only".to_string(),
                        display_name: "Client Only".to_string(),
                    }],
                    default_model: Some("client-only".to_string()),
                }),
            },
            active_source: Some(LlmModelSource::Client),
            selected_transport: None,
        }],
    };

    let frontend_catalog = assistant_provider_catalog(&catalog);
    assert_eq!(frontend_catalog.providers.len(), 1);
    let provider = &frontend_catalog.providers[0];
    assert_eq!(provider.model_source, AssistantModelSource::Client);
    assert!(!provider.available);
    assert_eq!(provider.models[0].id, "client-only");
}

#[test]
fn desktop_settings_bridge_persists_assistant_model_patch() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = AppCoreDesktopBridge::without_runtime(app.clone());

    bridge
        .apply_app_settings_patch(&AppSettingsPatch::AssistantModel(Some(
            "gpt-5.6-sol".to_string(),
        )))
        .unwrap();

    let result = app
        .relational
        .as_ref()
        .execute(
            RelationalOperation::GetPersistentSetting {
                scope: "frontend".to_string(),
                key: "app_settings".to_string(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let RelationalResult::PersistentSetting(record) = result else {
        panic!("unexpected relational result for app settings");
    };
    let Some(record) = record else {
        panic!("assistant model patch was not persisted");
    };
    assert_eq!(
        record.value["assistant_model"],
        serde_json::json!("gpt-5.6-sol")
    );
}

#[test]
fn desktop_settings_bridge_persists_host_voice_patch() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = AppCoreDesktopBridge::without_runtime(app.clone());

    bridge
        .apply_app_settings_patch(&AppSettingsPatch::VoiceRecordingEnabled(false))
        .unwrap();

    let settings = app.frontend().app_settings().unwrap();
    assert!(!settings.voice_recording_enabled);
}

#[test]
fn desktop_settings_bridge_restores_hidden_bulb_through_startup_delegate() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = AppCoreDesktopBridge::without_runtime(app.clone());
    bridge
        .apply_app_settings_patch(&AppSettingsPatch::BulbVisible(false))
        .unwrap();
    let restored = AppCoreDesktopBridge::without_runtime(app);
    assert!(!restored.bulb_visible().unwrap());
    restored
        .apply_app_settings_patch(&AppSettingsPatch::BulbVisible(true))
        .unwrap();
    assert!(restored.bulb_visible().unwrap());
}

#[test]
fn desktop_settings_bridge_saves_api_key_and_reloads_llm_registry() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = AppCoreDesktopBridge::without_runtime(app.clone());

    let result = bridge
        .set_provider_api_key("openrouter".to_string(), " test-key ".to_string())
        .unwrap();

    assert_eq!(result["providerId"], json!("openrouter"));
    assert_eq!(result["saved"], json!(true));
    assert_eq!(
        crate::app::provider_settings::stored_provider_api_key(
            app.relational.as_ref(),
            "openrouter"
        )
        .unwrap()
        .as_deref(),
        Some("test-key")
    );
    assert!(
        app.llms()
            .provider_ids()
            .unwrap()
            .contains(&"openrouter".to_string())
    );
}

#[test]
fn desktop_settings_bridge_saves_and_clears_custom_endpoint() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = AppCoreDesktopBridge::without_runtime(app.clone());

    let result = bridge
        .set_provider_endpoint(
            "custom_openai".to_string(),
            " http://localhost:11434/v1 ".to_string(),
        )
        .unwrap();

    assert_eq!(result["providerId"], json!("custom_openai"));
    assert_eq!(result["saved"], json!(true));
    assert_eq!(
        crate::app::provider_settings::stored_provider_endpoint(
            app.relational.as_ref(),
            "custom_openai"
        )
        .unwrap()
        .as_deref(),
        Some("http://localhost:11434/v1")
    );

    bridge
        .clear_provider_endpoint("custom_openai".to_string())
        .unwrap();
    assert_eq!(
        crate::app::provider_settings::stored_provider_endpoint(
            app.relational.as_ref(),
            "custom_openai"
        )
        .unwrap(),
        None
    );

    let error = bridge
        .set_provider_endpoint(
            "cerebras".to_string(),
            "https://api.cerebras.ai/v1".to_string(),
        )
        .unwrap_err();
    assert!(error.contains("custom_openai"));
}

#[test]
fn desktop_voice_bridge_encodes_renderer_samples_as_wav() {
    let wav = encode_pcm16_wav(vec![0.5, 0.0, -0.5], 16_000).unwrap();

    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 16_384);
}

#[test]
fn desktop_plugin_call_fails_closed_without_compiled_plugin() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = AppCoreDesktopBridge::without_runtime(app);

    let error = bridge
        .invoke_plugin_capability(
            "plugin.missing".to_string(),
            "capability".to_string(),
            json!({"input": "hello"}),
        )
        .expect_err("compiled Assistant is absent");

    assert!(error.contains("ready compiled plugin tool"));
}

#[test]
fn pending_desktop_bridge_delegates_after_install() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = PendingDesktopBridge::new();

    let not_ready =
        bridge.apply_app_settings_patch(&AppSettingsPatch::VoiceRecordingEnabled(false));
    assert!(
        not_ready
            .expect_err("patch should not apply while startup is queued")
            .contains("app is starting; request `apply_app_settings_patch`")
    );

    let delegate = Arc::new(AppCoreDesktopBridge::without_runtime(app.clone()));
    bridge
        .install(delegate)
        .expect("installing pending bridge delegate");

    bridge
        .apply_app_settings_patch(&AppSettingsPatch::VoiceRecordingEnabled(false))
        .expect("delegated patch should apply after install");
    let settings = app.frontend().app_settings().unwrap();
    assert!(!settings.voice_recording_enabled);
}

#[test]
fn pending_desktop_bridge_queues_startup_phase_actions_before_install() {
    let app = Arc::new(AppCore::in_memory().unwrap());
    let bridge = PendingDesktopBridge::new();

    bridge
        .record_startup_phase("db_open", Some("opening database"))
        .unwrap();
    bridge
        .record_startup_phase("plugins_ready", Some("plugins loaded"))
        .unwrap();
    bridge
        .record_startup_phase("voice_ready", Some("voice ready"))
        .unwrap();
    bridge
        .record_startup_phase("ready", Some("startup complete"))
        .unwrap();

    let pending = bridge.drain_frontend_actions().unwrap();
    assert_eq!(pending[0]["phase"], json!("db_open"));
    assert_eq!(pending[1]["phase"], json!("plugins_ready"));
    assert_eq!(pending[2]["phase"], json!("voice_ready"));
    assert_eq!(pending[3]["phase"], json!("ready"));

    bridge
        .install(Arc::new(AppCoreDesktopBridge::without_runtime(app.clone())))
        .expect("pending startup delegate install");
    let delivered = app.drain_frontend_actions().unwrap();
    assert_eq!(delivered[0]["phase"], json!("db_open"));
    assert_eq!(delivered[1]["phase"], json!("plugins_ready"));
    assert_eq!(delivered[2]["phase"], json!("voice_ready"));
    assert_eq!(delivered[3]["phase"], json!("ready"));
}

#[test]
fn pending_desktop_bridge_mutating_calls_return_starting_error() {
    let bridge = PendingDesktopBridge::new();

    let compiled_views = bridge
        .compiled_plugin_views()
        .expect("compiled views call should return empty catalog before startup");
    assert_eq!(compiled_views["baseUrl"], serde_json::Value::Null);
    assert_eq!(compiled_views["views"], json!([]));

    let snippet_error = bridge
        .submit_voice_snippet(json!({"sampleRate": 16_000, "samples": []}))
        .expect_err("voice snippet should be rejected before startup");

    let transcribe_error = bridge
        .transcribe_audio("test".into(), Vec::new(), 16_000, None)
        .expect_err("transcribe should be rejected before startup");

    let synthesize_error = bridge
        .synthesize_speech("test".into(), "hello".into(), None, None)
        .expect_err("synthesize should be rejected before startup");

    let mut dropped_events = Vec::new();
    let stream_error = bridge
        .stream_synthesize_speech("test".into(), "hello".into(), None, &mut |event| {
            dropped_events.push(event);
            Ok(())
        })
        .expect_err("streaming should be rejected before startup");

    let broadcast_error = bridge
        .record_screen_frame_broadcast("frame".into(), "image/png".into(), Vec::new())
        .expect_err("recording should be rejected before startup");

    let invoke_error = bridge
        .invoke_compiled_view_host_api(
            "plugin-id".into(),
            "view-id".into(),
            "api-id".into(),
            json!({"test": true}),
        )
        .expect_err("compiled view host api should be rejected before startup");

    let sync_canvas_error = bridge
        .sync_user_canvas_scene("session".into(), None, "{}".into())
        .expect_err("canvas sync should be rejected before startup");

    let provider_error = bridge
        .set_provider_api_key("openrouter".into(), "token".into())
        .expect_err("provider API key should be rejected before startup");

    assert!(dropped_events.is_empty());
    assert!(compiled_views["baseUrl"].is_null());
    assert!(snippet_error.contains("request `submit_voice_snippet`"));
    assert!(transcribe_error.contains("request `transcribe_audio`"));
    assert!(synthesize_error.contains("request `synthesize_speech`"));
    assert!(stream_error.contains("request `stream_synthesize_speech`"));
    assert!(broadcast_error.contains("request `record_screen_frame_broadcast`"));
    assert!(invoke_error.contains("request `invoke_compiled_view_host_api`"));
    assert!(sync_canvas_error.contains("request `sync_user_canvas_scene`"));
    assert!(provider_error.contains("request `set_provider_api_key`"));
}

#[test]
fn pending_desktop_bridge_voice_not_ready_yet() {
    let bridge = PendingDesktopBridge::new();

    let error = bridge
        .submit_voice_snippet(json!({"sampleRate": 16_000, "samples": []}))
        .expect_err("snippet calls should wait for startup");
    assert_eq!(
        error,
        "app is starting; request `submit_voice_snippet` is unavailable until startup completes"
    );
}

fn graph_request(
    provider_id: Option<&str>,
    project_root: Option<&str>,
) -> DesktopSemanticGraphRequest {
    DesktopSemanticGraphRequest {
        provider_id: provider_id.map(str::to_string),
        project_root: project_root.map(str::to_string),
        target_path: None,
        granularity: lumvise_db_core::SemanticGraphGranularity::File,
        recursive: true,
        include_external: false,
        include_first_neighbors: false,
    }
}

#[test]
fn pending_semantic_graph_bridge_reports_startup_error() {
    let bridge = PendingDesktopBridge::new();

    let roots_error = bridge
        .list_indexed_semantic_graph_roots()
        .expect_err("roots should wait for startup");
    let graph_error = bridge
        .project_indexed_semantic_graph(graph_request(Some("/repo"), None))
        .expect_err("graph should wait for startup");

    assert!(roots_error.contains("request `list_indexed_semantic_graph_roots`"));
    assert!(graph_error.contains("request `project_indexed_semantic_graph`"));
}

#[test]
fn semantic_graph_bridge_resolves_provider_and_root_identity() {
    let bridge = AppCoreDesktopBridge::without_runtime(Arc::new(AppCore::in_memory().unwrap()));

    let provider_only = bridge
        .project_indexed_semantic_graph(graph_request(Some("/provider"), None))
        .unwrap();
    let root_only = bridge
        .project_indexed_semantic_graph(graph_request(None, Some("/root")))
        .unwrap();
    let matching = bridge
        .project_indexed_semantic_graph(graph_request(Some("/same"), Some("/same")))
        .unwrap();
    let mismatch = bridge
        .project_indexed_semantic_graph(graph_request(Some("/provider"), Some("/root")))
        .expect_err("mismatched identities must be rejected");

    assert_eq!(provider_only.project_root, "/provider");
    assert_eq!(root_only.project_root, "/root");
    assert_eq!(matching.project_root, "/same");
    assert!(mismatch.contains("providerId must equal projectRoot"));
}

#[test]
fn semantic_graph_bridge_preserves_projection_defaults_and_result_shape() {
    let bridge = AppCoreDesktopBridge::without_runtime(Arc::new(AppCore::in_memory().unwrap()));
    let request = graph_request(None, Some("/missing"));
    assert!(request.recursive);
    assert!(!request.include_external);
    assert!(!request.include_first_neighbors);

    let projection = bridge.project_indexed_semantic_graph(request).unwrap();

    assert_eq!(projection.project_root, "/missing");
    assert!(projection.nodes.is_empty());
    assert!(projection.edge_source_indices.is_empty());
}

#[test]
fn semantic_graph_bridge_lists_only_indexed_roots() {
    let bridge = AppCoreDesktopBridge::without_runtime(Arc::new(AppCore::in_memory().unwrap()));
    assert!(
        bridge
            .list_indexed_semantic_graph_roots()
            .unwrap()
            .is_empty()
    );
}
