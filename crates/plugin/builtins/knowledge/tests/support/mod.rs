use std::path::Path;
use std::time::Duration;

use lumvise_plugin_runtime::{
    PluginInvocationClass, PluginInvocationContext, PluginInvocationRequest, PluginRuntimeConfig,
    PluginRuntimeError, PluginSystem,
};

pub(crate) trait ControlledTestInvoke {
    fn invoke(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: serde_json::Value,
    ) -> Result<lumvise_plugin_protocol::WireOutcome, PluginRuntimeError>;
}

impl ControlledTestInvoke for PluginSystem {
    fn invoke(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: serde_json::Value,
    ) -> Result<lumvise_plugin_protocol::WireOutcome, PluginRuntimeError> {
        let context = PluginInvocationContext::new(
            format!("knowledge-test-{export_id}"),
            "knowledge-test-owner",
            PluginInvocationClass::Foreground,
            std::time::Instant::now() + Duration::from_secs(3),
        );
        self.invoke_controlled(PluginInvocationRequest::new(
            plugin_id, export_id, input, context,
        ))
        .map_err(|error| error.into_runtime_error())
    }
}

pub(crate) fn fast_runtime_config() -> PluginRuntimeConfig {
    let mut config =
        PluginRuntimeConfig::default().with_controlled_test_deadline(Duration::from_secs(3));
    config.handshake_timeout = Duration::from_secs(3);
    config.shutdown_grace = Duration::from_secs(1);
    config
}

#[cfg(unix)]
pub(crate) fn make_tree_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            make_tree_writable(&path);
        }
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
pub(crate) fn make_tree_writable(_root: &Path) {}
