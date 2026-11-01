//! Public Semantic indexing entrypoint; App Core owns job scheduling and source I/O.
use lumvise_plugin_package::ExportDescriptor;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::Value;

/// Shared folder-indexing API, available through Semantic's compiled exports.
pub const INDEX_PROJECT_EXPORT_ID: &str = "index_project";

pub(crate) fn descriptor() -> ExportDescriptor {
    serde_json::from_str(include_str!("indexing-export.json"))
        .expect("checked-in Semantic indexing descriptor")
}

pub(crate) fn invoke(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    crate::indexing_request::index_project(input, context)
}
