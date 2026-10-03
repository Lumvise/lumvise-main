//! Owns the read-only document preference supplied to compiled importers.
//! Each import reads the current frontend preference; no process-local copy is authoritative.
use super::{PluginHostServices, failed};
use crate::plugin::host_capability_catalog::DOCUMENT_CONVERSION_OPTIONS;
use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Value, json};

impl PluginHostServices {
    pub(super) fn document_conversion_options(&self) -> Result<Value, HostCapabilityError> {
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| failed(DOCUMENT_CONVERSION_OPTIONS, "frontend mutex poisoned"))?;
        Ok(json!({
            "enhancement_enabled": frontend.app_settings().document_enhancement_enabled
        }))
    }
}
