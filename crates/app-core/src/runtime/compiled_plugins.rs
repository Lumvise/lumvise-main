//! Owns AppCore's compiled-plugin lifecycle and required base-plugin policy.
//! Callers use AppCore's registry/install/enable/disable/uninstall methods;
//! repository transitions and required-plugin classification remain internal.

use std::path::Path;

use lumvise_plugin_runtime::{PluginRegistry, PluginRegistryRecord};

use super::AppCore;
use crate::Result;

impl AppCore {
    /// Identifies required base plugins for host controls, e.g. `Self::is_required_compiled_plugin("builtin.semantic")`.
    pub(crate) fn is_required_compiled_plugin(plugin_id: &str) -> bool {
        matches!(plugin_id, "builtin.semantic" | "builtin.knowledge")
    }

    fn require_optional_compiled_plugin(plugin_id: &str) -> Result<()> {
        if Self::is_required_compiled_plugin(plugin_id) {
            return Err(crate::AppCoreError::invalid_value(
                plugin_id,
                "an optional plugin; required base plugins cannot be deactivated",
            ));
        }
        Ok(())
    }

    /// Returns durable compiled-plugin registry snapshot for production lifecycle calls.
    ///
    /// # Errors
    /// Returns an error for non-production App Core instances or poisoned state.
    pub fn compiled_plugin_registry(&self) -> Result<PluginRegistry> {
        Ok(self.production_plugin_repository()?.registry().clone())
    }

    /// Verifies and durably attaches one compiled plugin archive.
    ///
    /// When `enable` is true, process start and registry enablement form one
    /// rollback-safe transition.
    pub fn install_compiled_plugin(
        &self,
        archive: &Path,
        enable: bool,
    ) -> Result<PluginRegistryRecord> {
        let mut repository = self.production_plugin_repository()?;
        let record = repository.install_archive(archive, false)?;
        if !enable {
            return Ok(record);
        }
        if let Err(error) = repository.activate_with_validation(
            &self.plugin_system,
            &record.plugin_id,
            &record.version,
            || {
                self.validate_compiled_activation()
                    .map_err(|error| error.to_string())
            },
        ) {
            repository.uninstall(
                &record.plugin_id,
                &record.version,
                crate::CompiledPluginUninstallPolicy::Purge,
            )?;
            return Err(error.into());
        }
        Ok(PluginRegistryRecord {
            enabled: true,
            ..record
        })
    }

    /// Starts and selects one installed compiled-plugin version.
    pub fn enable_compiled_plugin(&self, plugin_id: &str, version: &str) -> Result<()> {
        self.production_plugin_repository()?
            .activate_with_validation(&self.plugin_system, plugin_id, version, || {
                self.validate_compiled_activation()
                    .map_err(|error| error.to_string())
            })?;
        Ok(())
    }

    /// Stops and durably disables an optional plugin, e.g. `app.disable_compiled_plugin("builtin.assistant")`.
    /// Required base plugins reject this transition without changing their state.
    pub fn disable_compiled_plugin(&self, plugin_id: &str) -> Result<()> {
        Self::require_optional_compiled_plugin(plugin_id)?;
        self.production_plugin_repository()?
            .deactivate(&self.plugin_system, plugin_id)?;
        self.sync_compiled_background_registrations()?;
        self.notify_background_registration_change()?;
        Ok(())
    }

    /// Detaches a version, e.g. `app.uninstall_compiled_plugin(id, version, policy)`.
    /// The enabled version of a required base plugin must remain installed.
    pub fn uninstall_compiled_plugin(
        &self,
        plugin_id: &str,
        version: &str,
        policy: crate::CompiledPluginUninstallPolicy,
    ) -> Result<()> {
        let mut repository = self.production_plugin_repository()?;
        let was_enabled = repository.registry().records.iter().any(|record| {
            record.plugin_id == plugin_id && record.version == version && record.enabled
        });
        if was_enabled {
            Self::require_optional_compiled_plugin(plugin_id)?;
            repository.deactivate(&self.plugin_system, plugin_id)?;
        }
        if let Err(error) = repository.uninstall(plugin_id, version, policy) {
            if was_enabled {
                let _ = repository.activate(&self.plugin_system, plugin_id, version);
            }
            return Err(error.into());
        }
        drop(repository);
        self.sync_compiled_background_registrations()?;
        self.notify_background_registration_change()?;
        Ok(())
    }

    fn sync_compiled_background_registrations(&self) -> Result<()> {
        self.plugin_endpoints()
            .sync_background_catalog(chrono::Utc::now().timestamp())?;
        Ok(())
    }

    fn validate_compiled_activation(&self) -> Result<()> {
        self.sync_compiled_background_registrations()?;
        self.notify_background_registration_change()
    }
}
