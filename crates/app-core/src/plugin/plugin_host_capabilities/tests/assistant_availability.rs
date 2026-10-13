//! Tests the public host capability used by app-owned Assistant sessions.
use super::*;

#[test]
fn assistant_engine_availability_allows_cloud_to_choose_its_own_model() {
    let services = services();
    services
        .frontend
        .lock()
        .unwrap()
        .apply_app_settings_patch(&AppSettingsPatch::AssistantEngine(
            AssistantEngine::LumviseCloud,
        ));
    let result = services
        .invoke(
            "builtin.assistant",
            ASSISTANT_ENGINE_AVAILABILITY,
            json!({}),
        )
        .unwrap();
    assert_eq!(result["available"], true);
    assert_eq!(
        services
            .frontend
            .lock()
            .unwrap()
            .app_settings()
            .assistant_model,
        None
    );
}

#[test]
fn assistant_engine_availability_still_requires_models_for_other_engines() {
    let services = services();
    for (engine, model, expected) in [
        (AssistantEngine::NativeMcp, Some("stale".to_owned()), false),
        (AssistantEngine::Codex, None, false),
        (
            AssistantEngine::Codex,
            Some("selected-model".to_owned()),
            true,
        ),
    ] {
        let mut frontend = services.frontend.lock().unwrap();
        frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantEngine(engine));
        frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantModel(model));
        drop(frontend);
        let result = services
            .invoke(
                "builtin.assistant",
                ASSISTANT_ENGINE_AVAILABILITY,
                json!({}),
            )
            .unwrap();
        assert_eq!(result["available"], expected, "engine {engine:?}");
    }
}
