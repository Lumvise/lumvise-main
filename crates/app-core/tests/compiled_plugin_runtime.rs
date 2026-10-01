use std::sync::Arc;

use lumvise_app_core::AppCore;
use lumvise_plugin_runtime::PluginSystem;

#[test]
fn app_core_owns_the_injected_compiled_plugin_system() {
    let plugin_system = Arc::new(PluginSystem::default());
    let app = AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system))
        .expect("in-memory database");

    assert!(std::ptr::eq(app.plugin_system(), plugin_system.as_ref()));
}

#[test]
fn app_core_default_constructor_owns_an_empty_compiled_plugin_system() {
    let app = AppCore::in_memory().expect("in-memory database");

    assert!(
        !app.plugin_system()
            .is_installed("plugin.not-installed")
            .expect("compiled plugin catalog")
    );
}
