//! Owns reviewed cross-project Knowledge copies. Signed preview/apply exports
//! are the public interface; matching and copy preparation remain internal.
//! Indexing never invokes apply. Example: preview a root, then apply its chosen IDs.
mod attachments;
mod references;

use crate::{
    KnowledgeArtifact,
    inheritance::{self, KnowledgeTransferMatch},
    semantic_context, storage,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRequest {
    project_root: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyRequest {
    project_root: String,
    transfer_ids: HashSet<String>,
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
        .map(|candidate| {
            candidate_value(
                candidate,
                completed_copy(&inventory, &candidate.copy.artifact_id),
            )
        })
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
    let inventory = transfer_inventory(&request.project_root, context)?;
    validate_selection(&request.transfer_ids, &inventory.candidates)?;
    let selected = inventory
        .candidates
        .iter()
        .filter(|candidate| request.transfer_ids.contains(&candidate.copy.artifact_id))
        .collect::<Vec<_>>();
    validate_destinations(&selected, context)?;
    let (copied, already) = copy_selected(&selected, &inventory, context)?;
    Ok(
        json!({"project_root": request.project_root, "copied_artifact_ids": copied,
        "already_copied_artifact_ids": already, "skipped_count": inventory.candidates.len() - selected.len()}),
    )
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
    let existing = snapshot
        .artifacts
        .into_iter()
        .map(|artifact| (artifact.artifact_id.clone(), artifact))
        .collect();
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
    selected: &HashSet<String>,
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
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    for candidate in selected {
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
        {
            return Err(PluginError::new(
                "transfer_destination_conflict",
                format!(
                    "artifact `{}` exists at `{}`; expected destination `{}`",
                    candidate.copy.artifact_id,
                    existing["semantic_element_id"],
                    candidate.target.semantic_element_id
                ),
                false,
            ));
        }
    }
    Ok(())
}

fn copy_selected(
    selected: &[&KnowledgeTransferMatch],
    inventory: &TransferInventory,
    context: &mut PluginContext<'_>,
) -> Result<(Vec<String>, Vec<String>), PluginError> {
    let reference_targets = inventory
        .candidates
        .iter()
        .filter(|candidate| {
            selected
                .iter()
                .any(|chosen| chosen.copy.artifact_id == candidate.copy.artifact_id)
                || completed_copy(inventory, &candidate.copy.artifact_id)
        })
        .collect::<Vec<_>>();
    let references = references::TransferReferences::new(&reference_targets);
    let mut prepared = Vec::new();
    let mut already = Vec::new();
    for candidate in selected {
        if completed_copy(inventory, &candidate.copy.artifact_id) {
            already.push(candidate.copy.artifact_id.clone());
            continue;
        }
        let mut copy = references.prepare(candidate)?;
        attachments::copy_referenced_attachments(&candidate.source, &mut copy, context)?;
        prepared.push(copy);
    }
    let copied = publish_copies(prepared, context)?;
    Ok((copied, already))
}

fn completed_copy(inventory: &TransferInventory, artifact_id: &str) -> bool {
    inventory
        .existing
        .get(artifact_id)
        .is_some_and(|artifact| artifact.metadata["inheritance"]["transfer_pending"] != true)
}

fn publish_copies(
    copies: Vec<KnowledgeArtifact>,
    context: &mut PluginContext<'_>,
) -> Result<Vec<String>, PluginError> {
    // Register every destination before its dependency edges. Selected copies
    // may refer to later IDs or cycles; an interrupted batch remains retryable.
    for copy in &copies {
        let mut staged = copy.clone();
        staged.dependencies.clear();
        staged.metadata["inheritance"]["transfer_pending"] = json!(true);
        storage::put_knowledge(context, &staged)?;
    }
    let mut completed = Vec::new();
    for copy in copies {
        storage::put_knowledge(context, &copy)?;
        completed.push(copy.artifact_id);
    }
    Ok(completed)
}
