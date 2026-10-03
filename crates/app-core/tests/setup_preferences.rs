use lumvise_app_core::AppCore;
use lumvise_db_core::{
    DbError, LocalPersistence, PersistenceResult, RelationalOperation, RelationalPersistence,
    RelationalReadiness, RelationalResult,
};
use lumvise_frontend_core::{AppSettingsPatch, FrontendCore};
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_resource_routing::InvocationControl;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct RejectNextSettingsWrite {
    persistence: Arc<LocalPersistence>,
    reject: AtomicBool,
}

impl RelationalPersistence for RejectNextSettingsWrite {
    fn execute(
        &self,
        operation: RelationalOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<RelationalResult> {
        if matches!(operation, RelationalOperation::SetPersistentSetting { .. })
            && self.reject.swap(false, Ordering::SeqCst)
        {
            return Err(DbError::invalid_value(
                "injected settings write failure",
                "durable settings write",
            ));
        }
        RelationalPersistence::execute(self.persistence.as_ref(), operation, control)
    }

    fn readiness(&self) -> PersistenceResult<RelationalReadiness> {
        RelationalPersistence::readiness(self.persistence.as_ref())
    }
}

#[test]
fn failed_finish_does_not_leak_completion_into_a_later_speech_save() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    let writes = Arc::new(RejectNextSettingsWrite {
        persistence: persistence.clone(),
        reject: AtomicBool::new(true),
    });
    let app = AppCore::new(
        persistence,
        writes,
        FrontendCore::default(),
        LlmProviderRegistry::empty(),
    );
    assert!(
        app.frontend()
            .apply_app_settings_patch(&AppSettingsPatch::SetupCompleted(true))
            .is_err()
    );
    assert!(!app.frontend().app_settings().unwrap().setup_completed);
    app.frontend()
        .apply_app_settings_patch(&AppSettingsPatch::SpeechRecognitionEnabled(false))
        .unwrap();
    let saved = app.frontend().app_settings().unwrap();
    assert!(!saved.setup_completed);
    assert!(!saved.speech_recognition_enabled);
    assert!(saved.speech_synthesis_enabled);
    app.frontend()
        .apply_app_settings_patch(&AppSettingsPatch::SetupCompleted(true))
        .unwrap();
    assert!(app.frontend().app_settings().unwrap().setup_completed);
}
