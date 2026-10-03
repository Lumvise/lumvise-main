//! Source-independent compiled Knowledge plugin.
//!
//! This crate owns Knowledge artifacts, search, cultivation, C4 reports, projection routes,
//! storage-event handling. Callers use only signed Plugin Protocol
//! exports; persistence and cross-plugin calls remain permission-gated Host Capabilities.

#![deny(missing_docs)]

mod artifact;
mod artifact_generation;
mod c4_report;
mod c4_text;
mod components;
mod cultivation;
mod events;
mod functional;
#[cfg(test)]
mod golden_tests;
mod http;
mod inheritance;
mod manifest;
mod md_nucleus;
mod obsidian_reports;
mod project_structure;
mod projection;
mod search;
mod semantic;
mod semantic_context;
mod storage;
mod transfer;

use lumvise_plugin_package::PluginManifest;
use lumvise_plugin_sdk::{PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};

pub use artifact::{KnowledgeArtifact, KnowledgeKind};
pub use manifest::{
    APPLY_TRANSFER_EXPORT_ID, ARTIFACT_GENERATION_POLL_EXPORT_ID,
    ASSISTANT_FIND_ELEMENTS_EXPORT_ID, ASSISTANT_GET_ELEMENT_EXPORT_ID, ASSISTANT_GET_EXPORT_ID,
    ASSISTANT_LIST_DEPENDENTS_EXPORT_ID, ASSISTANT_LIST_EXPORT_ID, ASSISTANT_SEARCH_EXPORT_ID,
    CREATE_EXPORT_ID, DEBUG_C4_EXPORT_ID, DELETE_EXPORT_ID, ELEMENT_TRIGGER_EXPORT_ID,
    ENSURE_C4_EXPORT_ID, FIND_ELEMENTS_EXPORT_ID, GET_CULTIVATION_RUN_EXPORT_ID,
    GET_ELEMENT_EXPORT_ID, GET_EXPORT_ID, HTTP_C4_ACTION_EXPORT_ID, HTTP_C4_DEBUG_EXPORT_ID,
    HTTP_C4_EXPORT_ID, HTTP_CREATE_ARTIFACT_EXPORT_ID, HTTP_DELETE_ARTIFACT_EXPORT_ID,
    HTTP_EVENTS_EXPORT_ID, HTTP_EXPORT_EXPORT_ID, HTTP_MANIFEST_EXPORT_ID, HTTP_PAGE_EXPORT_ID,
    HTTP_PROJECTION_ARTIFACTS_EXPORT_ID, HTTP_RESOLVE_TARGET_EXPORT_ID, HTTP_SETUP_EXPORT_ID,
    HTTP_SYNC_EXPORT_ID, HTTP_UPDATE_ARTIFACT_EXPORT_ID, HTTP_WRITE_EXPORT_ID, LIST_ALL_EXPORT_ID,
    LIST_DEPENDENTS_EXPORT_ID, LIST_EXPORT_ID, MANIFEST_EXPORT_ID, PACKAGE_PROTOCOL_VERSION,
    PLUGIN_ID, PREVIEW_TRANSFER_EXPORT_ID, PROJECTION_EXPORT_ID, REBUILD_EXPORT_ID,
    RUN_CULTIVATION_EXPORT_ID, SEARCH_EXPORT_ID, UPDATE_EXPORT_ID,
};

/// Creates canonical signed package metadata for offline `.lvp` tooling.
///
/// # Example
/// ```
/// let manifest = lumvise_plugin_knowledge::package_manifest_source(
///     "aarch64-apple-darwin",
///     &"0".repeat(64),
/// );
/// assert_eq!(manifest.plugin_id, "builtin.knowledge");
/// ```
pub fn package_manifest_source(target: &str, executable_sha256: &str) -> PluginManifest {
    manifest::source(target, executable_sha256)
}

/// Compiled Knowledge application with process-local projection snapshot state.
#[derive(Clone, Default)]
pub struct KnowledgePlugin {
    projection_cache: std::sync::Arc<projection::ProjectProjectionCache>,
}

impl PluginApplication for KnowledgePlugin {
    fn plugin_id(&self) -> &str {
        PLUGIN_ID
    }

    fn dispatch(
        &self,
        capability_id: &str,
        input: Value,
        context: &mut PluginContext<'_>,
    ) -> Result<Value, PluginError> {
        manifest::invoke(capability_id, input, context, &self.projection_cache)
    }
}

pub(crate) fn create(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: lumvise_contracts::CreateKnowledgeArtifactRequestV2 =
        parse(input, "Knowledge create request")?;
    let element = storage::required_semantic_element(context, &request.semantic_element_id)?;
    let mut artifact = artifact::create(request)?;
    attach_project_scope(&mut artifact, &element)?;
    storage::put_knowledge(context, &artifact)?;
    Ok(json!({"artifact": artifact}))
}

/// Reads a compiled Semantic element's `project_root` with explicit shape
/// validation - a malformed or missing value is a hard error, never a silent
/// empty-string default.
fn element_project_root(element: &Value, semantic_element_id: &str) -> Result<String, PluginError> {
    element["project_root"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            PluginError::new(
                "invalid_semantic_element",
                format!(
                    "semantic element `{semantic_element_id}` has a malformed project_root; \
                     expected a non-empty string"
                ),
                false,
            )
        })
}

fn attach_project_scope(
    artifact: &mut KnowledgeArtifact,
    element: &Value,
) -> Result<(), PluginError> {
    let owner = element_project_root(element, &artifact.semantic_element_id)?;
    if let Some(declared) = artifact.project_root.as_deref() {
        if declared != owner {
            return Err(PluginError::new(
                "knowledge_project_mismatch",
                format!(
                    "artifact `{}` declares project `{declared}`; expected anchor project `{owner}`",
                    artifact.artifact_id
                ),
                false,
            ));
        }
    }
    artifact.project_root = Some(owner);
    Ok(())
}

fn update(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: lumvise_contracts::UpdateKnowledgeArtifactRequestV2 =
        parse(input, "Knowledge update request")?;
    artifact::require(&request.artifact_id, "artifact_id")?;
    let current = required_artifact(context, &request.artifact_id)?;
    let requested_project = request.project_root.clone();
    let changed_anchor = request.semantic_element_id.is_some();
    let validate_scope = changed_anchor || requested_project.is_some();
    let mut artifact = artifact::update(current, request)?;
    if changed_anchor {
        artifact.project_root = requested_project;
    }
    if validate_scope {
        let element = storage::required_semantic_element(context, &artifact.semantic_element_id)?;
        attach_project_scope(&mut artifact, &element)?;
    }
    storage::put_knowledge(context, &artifact)?;
    Ok(json!({"artifact": artifact}))
}

fn delete(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let artifact_id = required_input(&input, "artifact_id")?;
    let project_root = required_input(&input, "project_root")?;
    let Some(artifact) = storage::get_knowledge(context, &artifact_id)? else {
        return Ok(json!({"artifact_id": artifact_id, "deleted": false}));
    };
    if artifact.project_root.as_deref() != Some(&project_root) {
        return Err(PluginError::new(
            "knowledge_project_mismatch",
            format!(
                "Knowledge artifact `{artifact_id}` belongs to `{:?}`; expected project_root `{project_root}`",
                artifact.project_root
            ),
            false,
        ));
    }
    if matches!(&artifact.knowledge_type, &KnowledgeKind::Report)
        && artifact.tags.iter().any(|tag| tag == "scoped-c4")
    {
        let target_id = artifact.metadata["nucleus"]["target_element_id"]
            .as_str()
            .unwrap_or(&artifact.semantic_element_id);
        cultivation::forget_c4_report_intent(&project_root, target_id, context)?;
    }
    let deleted = storage::remove_knowledge(context, &artifact_id)?;
    Ok(json!({"artifact_id": artifact_id, "deleted": deleted}))
}

fn get(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let artifact_id = required_input(&input, "artifact_id")?;
    Ok(json!({"artifact": required_artifact(context, &artifact_id)?}))
}

fn list(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let semantic_element_id = required_input(&input, "semantic_element_id")?;
    storage::required_semantic_element(context, &semantic_element_id)?;
    let mut artifacts = storage::artifacts_for_elements(
        context,
        &std::collections::HashSet::from([semantic_element_id]),
    )?;
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    Ok(json!({"artifacts": artifacts}))
}

/// Lists every Knowledge artifact, optionally filtered by project scope,
/// knowledge type, tags (any of), and a result limit — the same records
/// `GET /api/knowledge/projection-artifacts` returns.
fn list_all(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: artifact::ListAllRequest = parse(input, "Knowledge list request")?;
    let records = if let Some(ids) = &request.semantic_element_ids {
        for id in ids {
            artifact::require(id, "semantic_element_ids item")?;
        }
        storage::artifacts_for_elements(context, ids)?
    } else {
        match request.project_root.as_deref() {
            Some(root) => storage::list_project_knowledge(context, root)?,
            None => storage::list_all_knowledge(context)?,
        }
    };
    let artifacts = artifact::filter_artifacts(records, &request);
    Ok(json!({"artifacts": artifacts}))
}

fn list_dependents(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let target_kind = required_input(&input, "target_kind")?;
    let target_id = required_input(&input, "target_id")?;
    if !matches!(target_kind.as_str(), "semantic_element" | "artifact") {
        return Err(PluginError::new(
            "invalid_knowledge_input",
            format!("invalid `target_kind` value `{target_kind}`"),
            false,
        ));
    }
    let artifacts = storage::list_knowledge_dependents(context, &target_kind, &target_id)?;
    Ok(json!({"artifacts": artifacts}))
}

fn search(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: artifact::SearchRequest = parse(input, "Knowledge search request")?;
    artifact::require(&request.query, "query")?;
    search::search(&request, context)
}

fn get_element(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let element_id = required_input(&input, "semantic_element_id")?;
    let element = storage::required_semantic_element(context, &element_id)?;
    Ok(json!({"element": storage::public_semantic_element(&element)}))
}

fn rebuild_vectors(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let project_root = input.get("project_root").and_then(Value::as_str);
    search::rebuild(project_root, context)
}

fn required_artifact(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<KnowledgeArtifact, PluginError> {
    storage::get_knowledge(context, artifact_id)?.ok_or_else(|| {
        PluginError::new(
            "knowledge_not_found",
            format!("Knowledge artifact `{artifact_id}` is missing; expected stored artifact"),
            false,
        )
    })
}

pub(crate) fn required_input(input: &Value, field: &str) -> Result<String, PluginError> {
    let value = input.get(field).and_then(Value::as_str).unwrap_or_default();
    artifact::require(value, field)?;
    Ok(value.into())
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(
    input: Value,
    expected: &str,
) -> Result<T, PluginError> {
    serde_json::from_value(input.clone()).map_err(|error| {
        PluginError::new(
            "invalid_knowledge_input",
            format!("invalid Knowledge input `{input}`; expected {expected}: {error}"),
            false,
        )
    })
}
