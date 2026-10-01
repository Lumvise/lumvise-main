//! Ready-only discovery for signed recurring tasks and storage triggers.

use lumvise_plugin_package::{BackgroundDeliveryPolicy, ExportSurface};

use super::PluginSystem;
use crate::{ExportDescriptor, PluginRuntimeError};

/// One ready compiled background export and its signed delivery contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedBackgroundExport {
    /// Stable signed plugin identity.
    pub plugin_id: String,
    /// Package-local export identity.
    pub export_id: String,
    /// Signed background surface metadata.
    pub kind: BackgroundExportKind,
}

/// Signed scheduling or event-filter contract for a background export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackgroundExportKind {
    /// Fixed-cadence scheduled delivery.
    RecurringTask {
        /// Cadence between scheduled deliveries.
        interval_seconds: u64,
        /// Retry, timeout, and dead-letter policy.
        delivery: BackgroundDeliveryPolicy,
    },
    /// Storage-change delivery selected by signed filters.
    StorageTrigger {
        /// Accepted storage event kinds.
        event_kinds: Vec<String>,
        /// Accepted entity kinds; empty accepts every entity kind.
        entity_kinds: Vec<String>,
        /// Retry, timeout, and dead-letter policy.
        delivery: BackgroundDeliveryPolicy,
    },
}

impl BackgroundExportKind {
    /// Returns the host-enforced delivery policy.
    pub fn delivery(&self) -> &BackgroundDeliveryPolicy {
        match self {
            Self::RecurringTask { delivery, .. } | Self::StorageTrigger { delivery, .. } => {
                delivery
            }
        }
    }
}

impl PluginSystem {
    /// Returns signed background exports from ready processes only.
    ///
    /// # Errors
    /// Returns [`PluginRuntimeError::CatalogPoisoned`] if catalog state is unavailable.
    pub fn background_exports(&self) -> Result<Vec<PublishedBackgroundExport>, PluginRuntimeError> {
        let mut exports = self
            .published_plugins()?
            .into_iter()
            .flat_map(background_exports_for_plugin)
            .collect::<Vec<_>>();
        exports.sort_by(|left, right| {
            left.plugin_id
                .cmp(&right.plugin_id)
                .then_with(|| left.export_id.cmp(&right.export_id))
        });
        Ok(exports)
    }
}

fn background_exports_for_plugin(plugin: super::PublishedPlugin) -> Vec<PublishedBackgroundExport> {
    plugin
        .exports
        .iter()
        .filter_map(|export| background_export(&plugin.plugin_id, export))
        .collect()
}

fn background_export(
    plugin_id: &str,
    export: &ExportDescriptor,
) -> Option<PublishedBackgroundExport> {
    let kind = match &export.surface {
        ExportSurface::RecurringTask {
            interval_seconds,
            delivery,
        } => BackgroundExportKind::RecurringTask {
            interval_seconds: *interval_seconds,
            delivery: delivery.clone(),
        },
        ExportSurface::StorageTrigger {
            event_kinds,
            entity_kinds,
            delivery,
        } => BackgroundExportKind::StorageTrigger {
            event_kinds: event_kinds.clone(),
            entity_kinds: entity_kinds.clone(),
            delivery: delivery.clone(),
        },
        _ => return None,
    };
    Some(PublishedBackgroundExport {
        plugin_id: plugin_id.to_owned(),
        export_id: export.id.clone(),
        kind,
    })
}
