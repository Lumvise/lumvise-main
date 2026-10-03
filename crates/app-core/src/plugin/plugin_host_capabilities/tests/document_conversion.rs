use super::*;
use crate::plugin::host_capability_catalog::DOCUMENT_CONVERSION_OPTIONS;

#[test]
fn document_conversion_options_follow_current_preference_on_every_call() {
    let services = services();
    for enabled in [true, false, true] {
        services
            .frontend
            .lock()
            .unwrap()
            .apply_app_settings_patch(&AppSettingsPatch::DocumentEnhancementEnabled(enabled));
        let result = services
            .invoke("builtin.canvas", DOCUMENT_CONVERSION_OPTIONS, json!({}))
            .unwrap();
        assert_eq!(result, json!({"enhancement_enabled": enabled}));
    }
}
