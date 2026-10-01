//! Source-independent compiled Semantic plugin.
//!
//! Owns semantic index persistence, graph queries, search, and renderer graph projection behind
//! signed Plugin Protocol exports. Core crates only see serialized capability contracts.

#![deny(missing_docs)]

mod analysis;
mod context;
mod graph;
mod http;
mod ingest;
mod manifest;
mod models;
mod query;
mod snapshot;
mod storage;
mod triggers;

use lumvise_plugin_package::PluginManifest;
use lumvise_plugin_sdk::{PluginApplication, PluginContext, PluginError};
pub use manifest::{
    ARTIFACT_TRIGGER_EXPORT_ID, CANCEL_SNAPSHOT_EXPORT_ID, CREATE_SNAPSHOT_EXPORT_ID,
    DEPENDENCY_TREE_EXPORT_ID, ELEMENT_AT_LOCATION_EXPORT_ID, ELEMENT_TRIGGER_EXPORT_ID,
    GRAPH_PROVIDERS_EXPORT_ID, INGEST_EXPORT_ID, LATEST_INDEX_LOG_EXPORT_ID, MANIFEST_EXPORT_ID,
    PACKAGE_PROTOCOL_VERSION, PLUGIN_ID, REBUILD_SEARCH_INDEX_EXPORT_ID,
    RECORD_INDEX_LOG_EXPORT_ID, SEARCH_EXPORT_ID, SEMANTIC_CONTEXT_EXPORT_ID,
    SEMANTIC_GRAPH_EXPORT_ID, SNAPSHOT_STATUS_EXPORT_ID, TREE_EXPORT_ID,
};
use serde_json::Value;

/// Creates canonical signed manifest source for offline package tooling.
///
/// # Example
/// ```
/// let manifest = lumvise_plugin_semantic::package_manifest_source(
///     "aarch64-apple-darwin",
///     &"0".repeat(64),
/// );
/// assert_eq!(manifest.plugin_id, "builtin.semantic");
/// ```
pub fn package_manifest_source(target: &str, executable_sha256: &str) -> PluginManifest {
    manifest::source(target, executable_sha256)
}

/// Stateless compiled Semantic application dispatched by Plugin SDK.
#[derive(Clone, Copy, Debug, Default)]
pub struct SemanticPlugin;

impl PluginApplication for SemanticPlugin {
    fn plugin_id(&self) -> &str {
        PLUGIN_ID
    }

    fn dispatch(
        &self,
        capability_id: &str,
        input: Value,
        context: &mut PluginContext<'_>,
    ) -> Result<Value, PluginError> {
        manifest::invoke(capability_id, input, context)
    }
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(
    input: Value,
    expected: &str,
) -> Result<T, PluginError> {
    serde_json::from_value(input.clone()).map_err(|error| {
        PluginError::new(
            "invalid_semantic_input",
            format!("invalid Semantic input `{input}`; expected {expected}: {error}"),
            false,
        )
    })
}

pub(crate) fn serialize_response<T: serde::Serialize>(value: T) -> Result<Value, PluginError> {
    serde_json::to_value(value).map_err(|error| {
        PluginError::new(
            "invalid_semantic_output",
            format!("failed to serialize Semantic response: {error}"),
            false,
        )
    })
}

pub(crate) fn require_non_empty(value: &str, field: &str) -> Result<(), PluginError> {
    if value.trim().is_empty() {
        return Err(PluginError::new(
            "invalid_semantic_input",
            format!("invalid Semantic input `{value}` for `{field}`; expected non-empty string"),
            false,
        ));
    }
    Ok(())
}
