//! Owns reviewed cross-project Knowledge copies. Signed preview/apply exports
//! are the public interface; matching and copy preparation remain internal.
//! Indexing never invokes apply. Example: preview a root, then apply its chosen IDs.
mod attachments;
mod publication;
mod references;

use crate::{
    KnowledgeArtifact,
    inheritance::{self, KnowledgeTransferMatch},
    semantic_context, storage,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRequest {
    project_root: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApplyRequest {
    project_root: String,
    transfer_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
}

struct TransferInventory {
    candidates: Vec<KnowledgeTransferMatch>,
    existing: HashMap<String, KnowledgeArtifact>,
}

pub(crate) fn preview(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: PreviewRequest = crate::parse(input, "Knowledge transfer preview {project_root}")?;
    let inventory = transfer_inventory(&request.project_root, context)?;
    let candidates = inventory
        .candidates
        .iter()
        .map(|candidate| candidate_value(candidate, completed_copy(&inventory, candidate)))
        .collect::<Vec<_>>();
    Ok(
        json!({"project_root": request.project_root, "existing_artifact_count": inventory.existing.len(), "candidates": candidates}),
    )
}

pub(crate) fn apply(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: ApplyRequest = crate::parse(
        input,
        "Knowledge transfer selection {project_root, transfer_ids}",
    )?;
    crate::artifact::require(&request.project_root, "project_root")?;
    let receipt = crate::publication_journal::execute(
        context,
        "artifact.transfer",
        &request,
        request.operation_id.as_deref(),
        |context, operation_id| publication::freeze(&request, operation_id, context).map(Some),
    )?;
    publication::response(&request.project_root, receipt.as_ref())
}

fn transfer_inventory(
    project_root: &str,
    context: &mut PluginContext<'_>,
) -> Result<TransferInventory, PluginError> {
    crate::artifact::require(project_root, "project_root")?;
    let snapshot = semantic_context::load_projection_snapshot(context, project_root)?;
    let candidates = inheritance::project_transfer_candidates(
        project_root,
        &snapshot.semantic.elements,
        context,
    )?;
    let existing = existing_copies(snapshot.artifacts)?;
    Ok(TransferInventory {
        candidates,
        existing,
    })
}

fn candidate_value(candidate: &KnowledgeTransferMatch, already_copied: bool) -> Value {
    json!({
        "transfer_id": candidate.copy.artifact_id,
        "source_artifact_id": candidate.source.artifact_id,
        "source_project_root": candidate.source.project_root,
        "source_semantic_element_id": candidate.source.semantic_element_id,
        "title": candidate.source.title, "knowledge_type": candidate.source.knowledge_type,
        "target_semantic_element_id": candidate.target.semantic_element_id,
        "target_name": candidate.target.name, "target_path": candidate.target.path,
        "exact_match": candidate.exact_match, "simhash_distance": candidate.simhash_distance,
        "already_copied": already_copied
    })
}

fn validate_selection(
    selected: &BTreeSet<String>,
    candidates: &[KnowledgeTransferMatch],
) -> Result<(), PluginError> {
    let eligible = candidates
        .iter()
        .map(|candidate| &candidate.copy.artifact_id)
        .collect::<HashSet<_>>();
    for id in selected {
        if !eligible.contains(id) {
            return Err(PluginError::new(
                "stale_transfer_selection",
                format!(
                    "transfer id `{id}` is unavailable; expected a current preview candidate for the destination project"
                ),
                false,
            ));
        }
    }
    Ok(())
}

fn validate_destinations(
    selected: &[&KnowledgeTransferMatch],
    inventory: &TransferInventory,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    for candidate in selected {
        if let Some(current) = inventory.existing.get(&candidate.copy.artifact_id) {
            if current.semantic_element_id != candidate.target.semantic_element_id
                || current.project_root.as_deref() != Some(candidate.target.project_root.as_str())
                || current.metadata["inheritance"]["source_artifact_id"]
                    != candidate.source.artifact_id
            {
                return Err(destination_conflict(candidate, &json!(current.artifact_id)));
            }
        }
        let response = context.host_call(
            "storage.semantic",
            json!({"operation": "artifact", "artifact_id": candidate.copy.artifact_id}),
        )?;
        let existing = &response["artifact"];
        if existing.is_null() {
            continue;
        }
        if existing["semantic_element_id"] != candidate.target.semantic_element_id
            || !existing["metadata"]["knowledge"].is_object()
            || existing["metadata"]["knowledge"]["project_root"] != candidate.target.project_root
            || existing["metadata"]["knowledge"]["metadata"]["inheritance"]["source_artifact_id"]
                != candidate.source.artifact_id
        {
            return Err(destination_conflict(
                candidate,
                &existing["semantic_element_id"],
            ));
        }
    }
    Ok(())
}

fn destination_conflict(candidate: &KnowledgeTransferMatch, existing: &Value) -> PluginError {
    PluginError::new(
        "transfer_destination_conflict",
        format!(
            "transfer `{}` conflicts with `{existing}`; expected its source provenance and destination `{}`",
            candidate.copy.artifact_id, candidate.target.semantic_element_id
        ),
        false,
    )
}

fn completed_copy(inventory: &TransferInventory, candidate: &KnowledgeTransferMatch) -> bool {
    inventory
        .existing
        .get(&candidate.copy.artifact_id)
        .is_some_and(|artifact| {
            artifact.metadata["inheritance"]["transfer_pending"] != true
                && artifact.semantic_element_id == candidate.target.semantic_element_id
                && artifact.metadata["inheritance"]["source_artifact_id"]
                    == candidate.source.artifact_id
                && artifact.project_root.as_deref() == Some(candidate.target.project_root.as_str())
        })
}

fn existing_copies(
    artifacts: Vec<KnowledgeArtifact>,
) -> Result<HashMap<String, KnowledgeArtifact>, PluginError> {
    let mut indexed = HashMap::new();
    for artifact in artifacts {
        let key = artifact.metadata["inheritance"]["transfer_id"]
            .as_str()
            .unwrap_or(&artifact.artifact_id)
            .to_owned();
        if let Some(previous) = indexed.insert(key.clone(), artifact) {
            return Err(PluginError::new(
                "transfer_destination_conflict",
                format!(
                    "transfer `{key}` has multiple selected copies, including `{}`; expected one explicit destination revision",
                    previous.artifact_id
                ),
                false,
            ));
        }
    }
    Ok(indexed)
}
