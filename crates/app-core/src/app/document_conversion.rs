//! Owns the configured converter shared by previews and project import workers.
use crate::AppCore;
use lumvise_project_indexer::{DocumentConversionOptions, DocumentConverter};
use std::sync::Arc;

pub(super) fn converter(app: &AppCore) -> Result<Arc<DocumentConverter>, String> {
    shared_converter(
        &app.plugin_host_services.frontend(),
        &app.document_converter,
    )
}

pub(crate) fn shared_converter(
    frontend: &std::sync::Mutex<lumvise_frontend_core::FrontendCore>,
    cache: &std::sync::Mutex<Arc<DocumentConverter>>,
) -> Result<Arc<DocumentConverter>, String> {
    let frontend = frontend.lock().map_err(|_| {
        "document frontend preferences poisoned; expected available settings".to_string()
    })?;
    let settings = frontend.app_settings();
    let options = DocumentConversionOptions {
        enhancement_enabled: settings.document_enhancement_enabled,
    };
    drop(frontend);
    let mut shared = cache.lock().map_err(|_| {
        "document converter poisoned; expected available conversion state".to_string()
    })?;
    if shared.options() != options {
        *shared = Arc::new(DocumentConverter::new(options));
    }
    Ok(Arc::clone(&shared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_frontend_core::AppSettingsPatch;

    #[test]
    fn setting_change_replaces_shared_converter_and_preserves_other_preferences() {
        let app = AppCore::in_memory().unwrap();
        let first = converter(&app).unwrap();
        assert!(first.options().enhancement_enabled);
        assert!(Arc::ptr_eq(&first, &converter(&app).unwrap()));
        let mut expected = app.frontend().app_settings().unwrap();
        expected.document_enhancement_enabled = false;
        app.frontend()
            .apply_app_settings_patch(&AppSettingsPatch::DocumentEnhancementEnabled(false))
            .unwrap();
        let disabled = converter(&app).unwrap();
        assert!(!disabled.options().enhancement_enabled);
        assert!(!Arc::ptr_eq(&first, &disabled));
        assert_eq!(expected, app.frontend().app_settings().unwrap());
    }
}
