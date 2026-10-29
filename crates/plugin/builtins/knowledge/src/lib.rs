//! Source-independent compiled Knowledge plugin.
//!
//! This crate owns Knowledge artifacts, search, cultivation, C4 reports, projection routes,
//! storage-event handling. Callers use only signed Plugin Protocol
//! exports; persistence and cross-plugin calls remain permission-gated Host Capabilities.

#![deny(missing_docs)]

mod artifact;
mod artifact_generation;
mod artifact_publication;
mod c4_report;
mod c4_text;
mod components;
mod cultivation;
mod documents;
mod events;
mod functional;
#[cfg(test)]
mod golden_tests;
mod http;
mod inheritance;
mod lifecycle;
mod manifest;
mod md_nucleus;
mod obsidian_reports;
mod project_structure;
mod projection;
mod projection_identity;
mod publication_journal;
mod publication_observation;
mod search;
mod semantic;
mod semantic_context;
mod source_revisions;
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
    search_vectors: std::sync::Arc<search::KnowledgeSearchVectors>,
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
        manifest::invoke(
            capability_id,
            input,
            context,
            &self.projection_cache,
            &self.search_vectors,
        )
    }
}

pub(crate) fn create(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request = parse(input, "Knowledge create request")?;
    artifact_publication::create(context, request)
}

fn update(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request = parse(input, "Knowledge update request")?;
    artifact_publication::update(context, request)
}

fn delete(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    artifact_publication::remove(context, input)
}

fn get(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let artifact_id = required_input(&input, "artifact_id")?;
    Ok(json!({"artifact": required_artifact(context, &artifact_id)?}))
}

fn list(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let semantic_element_id = required_input(&input, "semantic_element_id")?;
    let include_inactive = input["include_inactive"].as_bool();
    storage::required_semantic_element(context, &semantic_element_id)?;
    let mut artifacts = storage::observe_knowledge(context, |context| {
        storage::artifacts_for_elements(
            context,
            &std::collections::HashSet::from([semantic_element_id]),
            None,
        )
    })?;
    artifacts.retain(|artifact| artifact::is_discoverable(artifact, include_inactive));
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    Ok(json!({"artifacts": artifacts}))
}

/// Lists Knowledge artifacts, optionally filtered by project scope, knowledge
/// type, tags (any of), and a result limit. Inactive records are hidden unless
/// `include_inactive` is set; `GET /api/knowledge/projection-artifacts` stays unfiltered.
fn list_all(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: artifact::ListAllRequest = parse(input, "Knowledge list request")?;
    if request
        .semantic_element_ids
        .as_ref()
        .is_some_and(|ids| ids.is_empty())
    {
        return Ok(json!({"artifacts":[]}));
    }
    for id in request.semantic_element_ids.iter().flatten() {
        artifact::require(id, "semantic_element_ids item")?;
    }
    let records = storage::observe_knowledge(context, |context| {
        list_requested_artifacts(context, &request)
    })?;
    let artifacts = artifact::filter_artifacts(records, &request);
    Ok(json!({"artifacts": artifacts}))
}

fn list_requested_artifacts(
    context: &mut PluginContext<'_>,
    request: &artifact::ListAllRequest,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let records = if let Some(ids) = &request.semantic_element_ids {
        storage::artifacts_for_elements(context, ids, request.project_root.as_deref())?
    } else {
        match request.project_root.as_deref() {
            Some(root) => storage::list_project_knowledge(context, root)?,
            None => storage::list_all_knowledge(context)?,
        }
    };
    Ok(records)
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

fn search(
    input: Value,
    context: &mut PluginContext<'_>,
    vectors: &search::KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let request: artifact::SearchRequest = parse(input, "Knowledge search request")?;
    artifact::require(&request.query, "query")?;
    search::search(&request, context, vectors)
}

fn get_element(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let element_id = required_input(&input, "semantic_element_id")?;
    let element = storage::required_semantic_element(context, &element_id)?;
    Ok(json!({"element": storage::public_semantic_element(&element)}))
}

fn rebuild_vectors(
    input: Value,
    context: &mut PluginContext<'_>,
    vectors: &search::KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    let project_root = input.get("project_root").and_then(Value::as_str);
    search::rebuild(project_root, context, vectors)
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
