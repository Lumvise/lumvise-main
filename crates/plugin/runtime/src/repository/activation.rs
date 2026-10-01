//! Runtime attachment and caller validation before durable plugin selection.

use lumvise_plugin_package::InstalledPlugin;

use crate::PluginSystem;

use super::{
    PluginRepository, PluginRepositoryError, attach_runtime, check_selection, detach_runtime,
    invalid, not_registered, record_selection, restore_previous_runtime,
};

impl PluginRepository {
    /// Selects, starts, and durably enables one version as one rollback-safe transition.
    pub fn activate(
        &mut self,
        system: &PluginSystem,
        plugin_id: &str,
        version: &str,
    ) -> Result<(), PluginRepositoryError> {
        self.activate_with_validation(system, plugin_id, version, || Ok(()))
    }

    /// Validates the attached candidate before committing its selected version.
    ///
    /// Example: `repository.activate_with_validation(&system, id, version, || sync_catalog())?`.
    pub fn activate_with_validation<F>(
        &mut self,
        system: &PluginSystem,
        plugin_id: &str,
        version: &str,
        mut validate: F,
    ) -> Result<(), PluginRepositoryError>
    where
        F: FnMut() -> Result<(), String>,
    {
        let requested = self
            .registry
            .records
            .iter()
            .find(|record| record.plugin_id == plugin_id && record.version == version)
            .ok_or_else(|| not_registered(plugin_id, version))?;
        check_selection(&self.registry, plugin_id, version)
            .map_err(|message| invalid(&self.root, message))?;
        if requested.enabled && system.is_active(plugin_id)? {
            return validate().map_err(|message| validation_error(plugin_id, version, message));
        }
        let (candidate, previous) = self.prepare_activation(plugin_id, version)?;
        let attached =
            detach_runtime(system, plugin_id).and_then(|()| attach_runtime(system, &candidate));
        if let Err(error) = attached {
            return Err(self.rollback_activation(
                system,
                plugin_id,
                previous.as_ref(),
                &mut validate,
                error,
                false,
                false,
            ));
        }
        let finalized = validate()
            .map_err(|message| validation_error(plugin_id, version, message))
            .and_then(|()| self.commit_selection(plugin_id, version));
        if let Err(error) = finalized {
            return Err(self.rollback_activation(
                system,
                plugin_id,
                previous.as_ref(),
                &mut validate,
                error,
                true,
                true,
            ));
        }
        Ok(())
    }

    fn commit_selection(
        &mut self,
        plugin_id: &str,
        version: &str,
    ) -> Result<(), PluginRepositoryError> {
        let mut next = self.registry.clone();
        record_selection(&mut next, plugin_id, version)
            .map_err(|message| invalid(&self.root, message))?;
        self.commit(next)
    }

    fn prepare_activation(
        &self,
        plugin_id: &str,
        version: &str,
    ) -> Result<(InstalledPlugin, Option<InstalledPlugin>), PluginRepositoryError> {
        let requested = self
            .registry
            .records
            .iter()
            .find(|record| record.plugin_id == plugin_id && record.version == version)
            .ok_or_else(|| not_registered(plugin_id, version))?;
        let previous = self
            .registry
            .records
            .iter()
            .find(|record| record.plugin_id == plugin_id && record.enabled);
        let candidate = self.prepare_restore(requested)?;
        let previous = match previous {
            Some(record) if record.version == version => Some(candidate.clone()),
            Some(record) => Some(self.prepare_restore(record)?),
            None => None,
        };
        Ok((candidate, previous))
    }

    fn rollback_activation<F>(
        &self,
        system: &PluginSystem,
        plugin_id: &str,
        previous: Option<&InstalledPlugin>,
        validate: &mut F,
        original: PluginRepositoryError,
        detach_candidate: bool,
        resync: bool,
    ) -> PluginRepositoryError
    where
        F: FnMut() -> Result<(), String>,
    {
        let mut failures = restore_runtime(system, plugin_id, previous, detach_candidate);
        if resync {
            if let Err(message) = validate() {
                failures.push(format!("restoring prior catalog: {message}"));
            }
        }
        if failures.is_empty() {
            return original;
        }
        invalid(
            &self.root,
            format!(
                "plugin `{plugin_id}` activation failed: {original}; rollback failed: {}",
                failures.join("; ")
            ),
        )
    }
}

fn restore_runtime(
    system: &PluginSystem,
    plugin_id: &str,
    previous: Option<&InstalledPlugin>,
    detach_candidate: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    if detach_candidate {
        if let Err(error) = detach_runtime(system, plugin_id) {
            failures.push(format!("detaching candidate: {error}"));
        }
    }
    if let Err(error) = restore_previous_runtime(system, previous) {
        failures.push(format!("restoring previous runtime: {error}"));
    }
    failures
}

fn validation_error(plugin_id: &str, version: &str, message: String) -> PluginRepositoryError {
    PluginRepositoryError::ActivationValidation {
        plugin_id: plugin_id.to_owned(),
        version: version.to_owned(),
        message,
    }
}
