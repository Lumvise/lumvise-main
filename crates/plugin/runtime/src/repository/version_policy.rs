//! Durable version selection rules for the plugin repository.

use semver::{BuildMetadata, Version};

use super::PluginRegistry;

pub(super) fn parse_version(value: &str) -> Result<Version, String> {
    let mut version = Version::parse(value)
        .map_err(|_| format!("version `{value}`; expected a semantic plugin version"))?;
    version.build = BuildMetadata::EMPTY;
    Ok(version)
}

pub(super) fn check_selection(
    registry: &PluginRegistry,
    plugin_id: &str,
    version: &str,
) -> Result<(), String> {
    let requested = parse_version(version)?;
    let Some(floor) = registry.highest_enabled_versions.get(plugin_id) else {
        return Ok(());
    };
    if requested < parse_version(floor)? {
        return Err(format!(
            "plugin `{plugin_id}` version `{version}` is below highest enabled version `{floor}`"
        ));
    }
    Ok(())
}

pub(super) fn record_selection(
    registry: &mut PluginRegistry,
    plugin_id: &str,
    version: &str,
) -> Result<(), String> {
    check_selection(registry, plugin_id, version)?;
    for record in &mut registry.records {
        if record.plugin_id == plugin_id {
            record.enabled = record.version == version;
        }
    }
    registry
        .highest_enabled_versions
        .insert(plugin_id.to_owned(), version.to_owned());
    Ok(())
}

pub(super) fn should_enable_bundle(
    registry: &PluginRegistry,
    plugin_id: &str,
    version: &str,
) -> Result<bool, String> {
    let bundled = parse_version(version)?;
    // A bundle takes over only on first install or as an upgrade. Preserve
    // disabled intent, and keep stale bundles installed but inactive.
    let installed: Vec<_> = registry
        .records
        .iter()
        .filter(|record| record.plugin_id == plugin_id)
        .collect();
    if installed.is_empty() && !registry.highest_enabled_versions.contains_key(plugin_id) {
        return Ok(true);
    }
    let Some(selected) = installed.iter().find(|record| record.enabled) else {
        return Ok(false);
    };
    let selected = parse_version(&selected.version)?;
    let floor = registry
        .highest_enabled_versions
        .get(plugin_id)
        .map(|value| parse_version(value))
        .transpose()?
        .unwrap_or(selected.clone());
    Ok(bundled > selected && bundled > floor)
}
