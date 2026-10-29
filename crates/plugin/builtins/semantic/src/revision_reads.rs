//! Explicit selected-family reads for plugin callers; policy remains in storage's shared owner.
use lumvise_contracts::{SemanticRevisionFamilyQuery, SemanticRevisionFamilyRead};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

/// Resolves a batch at one commit without changing exact identities or any caller's editing base.
/// Example: hooks resolve old source references in `working` before reading current input bodies.
pub(crate) fn resolve(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let mut query: SemanticRevisionFamilyQuery =
        crate::parse(input, "an explicit revision family scope")?;
    crate::require_non_empty(&query.project_root, "project_root")?;
    crate::require_non_empty(&query.selection_id, "selection_id")?;
    let commit_version = match query.expected_commit_version {
        Some(version) => version,
        None => crate::storage::semantic_revision(context, &query.project_root)?,
    };
    query.expected_commit_version = Some(commit_version);
    let output = context.host_call(
        "storage.semantic",
        json!({"operation":"revision_families","query":query}),
    )?;
    crate::serialize_response(SemanticRevisionFamilyRead {
        commit_version,
        remaps: crate::parse(
            output["remaps"].clone(),
            "explicit selected revision remaps",
        )?,
    })
}
