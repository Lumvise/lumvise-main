//! Source-owned selection and explicit cleanup controls. Policy remains in semantic-revisions;
//! the host authorizes every semantic read/write and the persistence owner fences each change.
mod schemas;
mod settings;
#[cfg(test)]
mod tests;

use lumvise_plugin_sdk::{PluginContext, PluginError};
use lumvise_semantic_revisions::{
    SemanticRecordRef, SemanticRevisionRetentionPlan, SemanticRevisionRetentionPolicy,
    SemanticRevisionSelectionUpdate,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) use schemas::{exports, supports};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRead {
    project_root: String,
    selection_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactRead {
    project_root: String,
    record: SemanticRecordRef,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionPreview {
    project_root: String,
    operation_id: String,
    policy: SemanticRevisionRetentionPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionCollection {
    plan: SemanticRevisionRetentionPlan,
    policy: SemanticRevisionRetentionPolicy,
}

pub(crate) fn invoke(
    export: &str,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    match export {
        "get_revision_retention_preference" => settings::read(input, context),
        "set_revision_retention_preference" => settings::write(input, context),
        _ => context.host_call("storage.semantic", request(export, input)?),
    }
}

fn request(export: &str, input: Value) -> Result<Value, PluginError> {
    match export {
        "read_revision_selection" => selection_read(input),
        "select_revisions" => {
            let update: SemanticRevisionSelectionUpdate =
                crate::parse(input, "an exact conditional selection update")?;
            if update.selection_id.starts_with("knowledge-citation:") {
                return Err(PluginError::new(
                    "reserved_revision_selection",
                    format!(
                        "selection `{}` is citation-owned; expected a user-managed selection outside knowledge-citation:",
                        update.selection_id
                    ),
                    false,
                ));
            }
            Ok(json!({"operation":"select_revisions", "update":update}))
        }
        "resolve_revision" => exact_read(input),
        "preview_revision_retention" => retention_preview(input),
        "collect_revision_retention" => {
            let request: RetentionCollection =
                crate::parse(input, "the unchanged preview plan and policy")?;
            Ok(
                json!({"operation":"collect_revision_retention", "plan":request.plan, "policy":request.policy}),
            )
        }
        _ => Err(PluginError::unknown_capability(export)),
    }
}

fn selection_read(input: Value) -> Result<Value, PluginError> {
    let read: SelectionRead = crate::parse(input, "project_root and selection_id")?;
    crate::require_non_empty(&read.project_root, "project_root")?;
    crate::require_non_empty(&read.selection_id, "selection_id")?;
    Ok(
        json!({"operation":"revision_selection", "project_root":read.project_root, "selection_id":read.selection_id}),
    )
}

fn exact_read(input: Value) -> Result<Value, PluginError> {
    let read: ExactRead = crate::parse(input, "project_root and an exact record reference")?;
    crate::require_non_empty(&read.project_root, "project_root")?;
    Ok(
        json!({"operation":"resolve_revision", "project_root":read.project_root, "record":read.record}),
    )
}

fn retention_preview(input: Value) -> Result<Value, PluginError> {
    let preview: RetentionPreview =
        crate::parse(input, "project_root, operation_id and a retention policy")?;
    crate::require_non_empty(&preview.project_root, "project_root")?;
    crate::require_non_empty(&preview.operation_id, "operation_id")?;
    Ok(
        json!({"operation":"plan_revision_retention", "project_root":preview.project_root,
        "operation_id":preview.operation_id, "policy":preview.policy}),
    )
}
