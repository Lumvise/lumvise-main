//! Owns conditional Knowledge artifact edits and their frozen retry lifecycle.
//! Signed create/update/remove exports call these entrypoints; storage and journal details stay internal.
use crate::publication_journal::{self as journal, FrozenKnowledgePublication};

use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeArtifactRequestV2, SemanticIndexObservation, SemanticRecordRef,
    UpdateKnowledgeArtifactRequestV2,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{KnowledgeArtifact, artifact, storage};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoveArtifactRequest {
    artifact_id: String,
    project_root: String,
    expected_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
}

pub(crate) fn create(
    context: &mut PluginContext<'_>,
    request: CreateKnowledgeArtifactRequestV2,
) -> Result<Value, PluginError> {
    let receipt = journal::execute(
        context,
        "create",
        &request,
        request.operation_id.as_deref(),
        |context, operation_id| {
            prepare_create(context, &request, operation_id)
                .map(FrozenKnowledgePublication::ArtifactRevision)
                .map(Some)
        },
    )?;
    accepted_artifact(context, &request.artifact_id, receipt.as_ref())
}

pub(crate) fn update(
    context: &mut PluginContext<'_>,
    request: UpdateKnowledgeArtifactRequestV2,
) -> Result<Value, PluginError> {
    let receipt = journal::execute(
        context,
        "update",
        &request,
        request.operation_id.as_deref(),
        |context, operation_id| {
            prepare_update(context, &request, operation_id)
                .map(FrozenKnowledgePublication::ArtifactRevision)
                .map(Some)
        },
    )?;
    accepted_artifact(context, &request.artifact_id, receipt.as_ref())
}

pub(crate) fn remove(context: &mut PluginContext<'_>, input: Value) -> Result<Value, PluginError> {
    let request: RemoveArtifactRequest = crate::parse(input, "Knowledge artifact removal")?;
    artifact::require(&request.artifact_id, "artifact_id")?;
    artifact::require(&request.project_root, "project_root")?;
    let receipt = journal::execute(
        context,
        "remove",
        &request,
        request.operation_id.as_deref(),
        |context, operation_id| {
            prepare_remove(context, &request, operation_id)
                .map(|proposal| proposal.map(FrozenKnowledgePublication::ArtifactRevision))
        },
    )?;
    Ok(json!({"artifact_id":request.artifact_id, "deleted":receipt.is_some()}))
}

fn prepare_remove(
    context: &mut PluginContext<'_>,
    request: &RemoveArtifactRequest,
    operation_id: &str,
) -> Result<Option<Value>, PluginError> {
    if request.expected_revision == 0 {
        return prepare_absent_removal(context, request);
    }
    let current = storage::get_knowledge(context, &request.artifact_id)?.ok_or_else(|| {
        conflict(
            &request.artifact_id,
            "the original selected artifact edit counter, or 0 for an observed absence",
        )
    })?;
    validate_removal_read(&current, request)?;
    let observed = observe_artifact(context, &current)?;
    if observed.expected.records != vec![SemanticRecordRef::artifact(&request.artifact_id)] {
        return Err(conflict(
            &request.artifact_id,
            "the exact currently selected artifact revision",
        ));
    }
    let mut publication = publication_scope(&request.project_root, observed, operation_id);
    publication["removed_artifact_ids"] = json!([request.artifact_id]);
    Ok(Some(publication))
}

fn prepare_absent_removal(
    context: &mut PluginContext<'_>,
    request: &RemoveArtifactRequest,
) -> Result<Option<Value>, PluginError> {
    let observed = observe(context, &request.project_root, &request.artifact_id)?;
    if observed.expected.records.is_empty() {
        return Ok(None);
    }
    Err(conflict(
        &request.artifact_id,
        "no currently selected artifact for an observed absence",
    ))
}

fn validate_removal_read(
    current: &KnowledgeArtifact,
    request: &RemoveArtifactRequest,
) -> Result<(), PluginError> {
    if current.lifecycle.revision != request.expected_revision {
        return Err(conflict(
            &request.artifact_id,
            "expected_revision matching the original artifact read",
        ));
    }
    if project_root(current)? != request.project_root {
        return Err(PluginError::new(
            "knowledge_project_mismatch",
            format!(
                "Knowledge artifact `{}` belongs to `{:?}`; expected project_root `{}`",
                request.artifact_id, current.project_root, request.project_root
            ),
            false,
        ));
    }
    Ok(())
}

fn prepare_create(
    context: &mut PluginContext<'_>,
    request: &CreateKnowledgeArtifactRequestV2,
    operation_id: &str,
) -> Result<Value, PluginError> {
    artifact::require(&request.artifact_id, "artifact_id")?;
    let mut proposed = artifact::create(request.clone())?;
    let element = storage::required_semantic_element(context, &request.semantic_element_id)?;
    attach_project_scope(&mut proposed, &element)?;
    let stored = storage::raw_graph_artifact(context, &request.artifact_id)?;
    if let Some(stored) = stored
        && stored_in_project(context, &stored, project_root(&proposed)?)?
    {
        storage::validate_knowledge_owner(&proposed, Some(&stored))?;
        return Err(conflict(
            &request.artifact_id,
            "an unused artifact identity; update an existing revision explicitly",
        ));
    }
    let observed = observe(context, project_root(&proposed)?, &proposed.artifact_id)?;
    if !observed.expected.records.is_empty() {
        return Err(conflict(&proposed.artifact_id, "an unused artifact family"));
    }
    artifact::stamp(&mut proposed, None, Utc::now());
    publication(&proposed, observed, operation_id)
}

fn stored_in_project(
    context: &mut PluginContext<'_>,
    stored: &Value,
    root: &str,
) -> Result<bool, PluginError> {
    let owner_id = stored["semantic_element_id"]
        .as_str()
        .ok_or_else(|| invalid(stored, "a stored artifact's exact semantic owner ID"))?;
    let owner = storage::required_semantic_element(context, owner_id)?;
    Ok(owner["project_root"] == root)
}

fn prepare_update(
    context: &mut PluginContext<'_>,
    request: &UpdateKnowledgeArtifactRequestV2,
    operation_id: &str,
) -> Result<Value, PluginError> {
    artifact::require(&request.artifact_id, "artifact_id")?;
    let previous = crate::required_artifact(context, &request.artifact_id)?;
    let root = project_root(&previous)?.to_owned();
    let observed = observe_artifact(context, &previous)?;
    if observed.expected.records != vec![SemanticRecordRef::artifact(&request.artifact_id)] {
        return Err(conflict(
            &request.artifact_id,
            "the exact currently selected artifact revision",
        ));
    }
    if let Some(target) = request.superseded_by.as_deref() {
        crate::required_artifact(context, target)?;
    }
    let mut proposed = artifact::update(previous.clone(), request.clone())?;
    validate_updated_scope(context, &root, &mut proposed)?;
    artifact::stamp(&mut proposed, Some(&previous.lifecycle), Utc::now());
    publication(&proposed, observed, operation_id)
}

fn validate_updated_scope(
    context: &mut PluginContext<'_>,
    original_root: &str,
    proposed: &mut KnowledgeArtifact,
) -> Result<(), PluginError> {
    let element = storage::required_semantic_element(context, &proposed.semantic_element_id)?;
    attach_project_scope(proposed, &element)?;
    if project_root(proposed)? != original_root {
        return Err(conflict(
            &proposed.artifact_id,
            "an owner in the same project; use reviewed transfer for a different project",
        ));
    }
    Ok(())
}

fn observe_artifact(
    context: &mut PluginContext<'_>,
    original: &KnowledgeArtifact,
) -> Result<SemanticIndexObservation, PluginError> {
    let observed = observe(context, project_root(original)?, &original.artifact_id)?;
    if crate::required_artifact(context, &original.artifact_id)? != *original {
        return Err(conflict(
            &original.artifact_id,
            "the original artifact body throughout observation",
        ));
    }
    Ok(observed)
}

fn project_root(artifact: &KnowledgeArtifact) -> Result<&str, PluginError> {
    artifact
        .project_root
        .as_deref()
        .filter(|root| !root.is_empty())
        .ok_or_else(|| {
            invalid(
                &json!(artifact.artifact_id),
                "a stored non-empty project root",
            )
        })
}

fn attach_project_scope(
    artifact: &mut KnowledgeArtifact,
    element: &Value,
) -> Result<(), PluginError> {
    let owner = element["project_root"]
        .as_str()
        .filter(|root| !root.is_empty())
        .ok_or_else(|| invalid(element, "a semantic owner with non-empty project_root"))?;
    if let Some(declared) = artifact.project_root.as_deref()
        && declared != owner
    {
        return Err(PluginError::new(
            "knowledge_project_mismatch",
            format!(
                "artifact `{}` declares project `{declared}`; expected anchor project `{owner}`",
                artifact.artifact_id
            ),
            false,
        ));
    }
    artifact.project_root = Some(owner.into());
    Ok(())
}

fn observe(
    context: &mut PluginContext<'_>,
    root: &str,
    artifact_id: &str,
) -> Result<SemanticIndexObservation, PluginError> {
    crate::publication_observation::read(
        context,
        root,
        json!({"kind":"none"}),
        &[artifact_id.into()],
    )
}

fn publication(
    proposed: &KnowledgeArtifact,
    observed: SemanticIndexObservation,
    operation_id: &str,
) -> Result<Value, PluginError> {
    let mut publication = publication_scope(project_root(proposed)?, observed, operation_id);
    publication["artifacts"] = json!([{"artifact":storage::graph_artifact(proposed)?, "media_type":"text/markdown; charset=utf-8"}]);
    Ok(publication)
}

fn publication_scope(root: &str, observed: SemanticIndexObservation, operation_id: &str) -> Value {
    json!({
        "operation_id":operation_id, "project_root":root, "project_id":observed.project_id,
        "selection_id":observed.selection_id, "source_context":observed.source_context,
        "producer_base":null, "expected":observed.expected, "structure":null,
        "artifacts":[],
        "removed_artifact_ids":[], "source_anchor":null
    })
}

fn accepted_artifact(
    context: &mut PluginContext<'_>,
    incoming_id: &str,
    receipt: Option<&Value>,
) -> Result<Value, PluginError> {
    let accepted = accepted_artifact_id(incoming_id, receipt)?;
    let receipt = receipt.expect("accepted_artifact_id checked the durable receipt");
    let root = receipt["selection"]["project_root"]
        .as_str()
        .ok_or_else(|| invalid(receipt, "the accepted project root"))?;
    let version = receipt["accepted_scope"]["artifact_edits"][accepted]
        .as_u64()
        .ok_or_else(|| invalid(receipt, "the accepted artifact edit version"))?;
    let artifact = storage::artifact_at_edit(context, root, accepted, version)?;
    Ok(json!({"artifact":artifact}))
}

pub(crate) fn accepted_artifact_id<'a>(
    incoming_id: &str,
    receipt: Option<&'a Value>,
) -> Result<&'a str, PluginError> {
    let receipt = receipt
        .ok_or_else(|| invalid(&json!(incoming_id), "an accepted artifact write receipt"))?;
    let remap = artifact_receipt_remap(incoming_id, receipt)?;
    remap["accepted"]["record_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .filter(|_| remap["accepted"]["kind"] == "artifact")
        .ok_or_else(|| invalid(remap, "a non-empty exact accepted artifact revision"))
}

fn artifact_receipt_remap<'a>(
    incoming_id: &str,
    receipt: &'a Value,
) -> Result<&'a Value, PluginError> {
    receipt["remaps"]
        .as_array()
        .and_then(|remaps| {
            remaps.iter().find(|remap| {
                remap["incoming"]["kind"] == "artifact"
                    && remap["incoming"]["record_id"] == incoming_id
            })
        })
        .ok_or_else(|| {
            invalid(
                receipt,
                "an accepted artifact remap for the requested identity",
            )
        })
}

fn conflict(artifact_id: &str, expected: &str) -> PluginError {
    PluginError::new(
        "knowledge_revision_conflict",
        format!("Knowledge artifact `{artifact_id}` conflicts; expected {expected}"),
        false,
    )
}

pub(super) fn invalid(value: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_knowledge_publication",
        format!("invalid Knowledge publication `{value}`; expected {expected}"),
        false,
    )
}
