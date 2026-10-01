//! Ready-only discovery and surface-checked invocation for signed Commands.

use super::PluginSystem;
use crate::{ExportDescriptor, PluginRuntimeError};
use lumvise_plugin_package::ExportSurface;

/// One ready compiled Command declared by signed package metadata.
#[derive(Clone, Debug)]
pub struct PublishedCommand {
    /// Stable signed plugin identity.
    pub plugin_id: String,
    /// Full signed Command descriptor, including its schemas.
    pub export: ExportDescriptor,
}

impl PluginSystem {
    /// Returns signed Commands from ready plugin processes only.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] when catalog state is unavailable.
    pub fn command_exports(&self) -> Result<Vec<PublishedCommand>, PluginRuntimeError> {
        let mut commands = self
            .published_plugins()?
            .into_iter()
            .flat_map(commands_for_plugin)
            .collect::<Vec<_>>();
        commands.sort_by(|left, right| {
            left.plugin_id
                .cmp(&right.plugin_id)
                .then_with(|| left.export.id.cmp(&right.export.id))
        });
        Ok(commands)
    }
}

fn commands_for_plugin(plugin: super::PublishedPlugin) -> Vec<PublishedCommand> {
    plugin
        .exports
        .into_iter()
        .filter(|export| export.surface == ExportSurface::Command)
        .map(|export| PublishedCommand {
            plugin_id: plugin.plugin_id.clone(),
            export,
        })
        .collect()
}
