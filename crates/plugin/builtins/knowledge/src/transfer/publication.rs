//! Freezes reviewed copy identities, dependencies and results; shared Rust owns acceptance.
use super::*;
use crate::publication_journal::FrozenKnowledgePublication;
use lumvise_contracts::{SemanticIndexObservation, SemanticRecordRef};

#[derive(Deserialize, Serialize)]
struct TransferCompletion {
    copied: Vec<String>,
    already: Vec<String>,
    skipped_count: usize,
}

struct TransferPublicationDraft<'a> {
    request: &'a ApplyRequest,
    inventory: &'a TransferInventory,
    selected: Vec<&'a KnowledgeTransferMatch>,
    destinations: HashMap<String, TransferDestination>,
    observation: SemanticIndexObservation,
    operation_id: &'a str,
}

struct TransferDestination {
    incoming_id: String,
    exact_id: String,
    expected_current: Option<String>,
}

pub(super) fn freeze(
    request: &ApplyRequest,
    operation_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<FrozenKnowledgePublication, PluginError> {
    let inventory = transfer_inventory(&request.project_root, context)?;
    TransferPublicationDraft::capture(request, &inventory, operation_id, context)?.prepare(context)
}

impl<'a> TransferPublicationDraft<'a> {
    fn capture(
        request: &'a ApplyRequest,
        inventory: &'a TransferInventory,
        operation_id: &'a str,
        context: &mut PluginContext<'_>,
    ) -> Result<Self, PluginError> {
        let selected = selected_matches(request, inventory)?;
        validate_destinations(&selected, inventory, context)?;
        let project_id =
            crate::publication_observation::project_id(context, &request.project_root)?;
        let destinations = destination_ids(request, inventory, &project_id, operation_id, context)?;
        let observation = capture_scope(request, inventory, &selected, &destinations, context)?;
        require_lineage(&observation, &project_id)?;
        Ok(Self {
            request,
            inventory,
            selected,
            destinations,
            observation,
            operation_id,
        })
    }

    fn prepare(
        self,
        context: &mut PluginContext<'_>,
    ) -> Result<FrozenKnowledgePublication, PluginError> {
        let references = self.references();
        let already = self.completed_copies();
        let (copied, artifacts) = self
            .prepare_copies(context, &references)?
            .into_iter()
            .unzip();
        Ok(self.finish(artifacts, copied, already))
    }

    fn completed_copies(&self) -> Vec<String> {
        self.selected
            .iter()
            .filter(|candidate| completed_copy(self.inventory, candidate))
            .map(|candidate| {
                self.inventory.existing[&candidate.copy.artifact_id]
                    .artifact_id
                    .clone()
            })
            .collect()
    }

    fn prepare_copies(
        &self,
        context: &mut PluginContext<'_>,
        references: &references::TransferReferences,
    ) -> Result<Vec<(String, Value)>, PluginError> {
        self.selected
            .iter()
            .filter(|candidate| !completed_copy(self.inventory, candidate))
            .map(|candidate| {
                prepare_write(
                    candidate,
                    references,
                    &self.destinations[&candidate.copy.artifact_id].incoming_id,
                    context,
                )
            })
            .collect()
    }

    fn references(&self) -> references::TransferReferences {
        let targets = self
            .inventory
            .candidates
            .iter()
            .filter(|candidate| self.destinations.contains_key(&candidate.copy.artifact_id))
            .collect::<Vec<_>>();
        let exact_ids = self
            .destinations
            .iter()
            .map(|(key, destination)| (key.clone(), destination.exact_id.clone()))
            .collect();
        references::TransferReferences::new(&targets, &exact_ids)
    }

    fn finish(
        self,
        artifacts: Vec<Value>,
        copied: Vec<String>,
        already: Vec<String>,
    ) -> FrozenKnowledgePublication {
        let publication = json!({"operation_id":self.operation_id, "project_root":self.request.project_root,
            "project_id":self.observation.project_id, "selection_id":self.observation.selection_id,
            "source_context":self.observation.source_context, "producer_base":null, "expected":self.observation.expected,
            "structure":null, "artifacts":artifacts, "removed_artifact_ids":[], "source_anchor":null});
        let completion = json!(TransferCompletion {
            copied,
            already,
            skipped_count: self.inventory.candidates.len() - self.selected.len()
        });
        FrozenKnowledgePublication::ArtifactTransfer {
            publication,
            completion,
        }
    }
}

fn require_lineage(
    observation: &SemanticIndexObservation,
    project_id: &str,
) -> Result<(), PluginError> {
    if observation.project_id == project_id {
        return Ok(());
    }
    Err(invalid(
        &json!(observation.project_id),
        "the original destination project lineage",
    ))
}

fn selected_matches<'a>(
    request: &ApplyRequest,
    inventory: &'a TransferInventory,
) -> Result<Vec<&'a KnowledgeTransferMatch>, PluginError> {
    validate_selection(&request.transfer_ids, &inventory.candidates)?;
    Ok(inventory
        .candidates
        .iter()
        .filter(|candidate| request.transfer_ids.contains(&candidate.copy.artifact_id))
        .collect())
}

fn destination_ids(
    request: &ApplyRequest,
    inventory: &TransferInventory,
    project_id: &str,
    operation_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<HashMap<String, TransferDestination>, PluginError> {
    inventory
        .candidates
        .iter()
        .filter(|candidate| {
            completed_copy(inventory, candidate)
                || request.transfer_ids.contains(&candidate.copy.artifact_id)
        })
        .map(|candidate| {
            destination_choice(
                request,
                inventory,
                candidate,
                project_id,
                operation_id,
                context,
            )
        })
        .collect()
}

fn destination_choice(
    request: &ApplyRequest,
    inventory: &TransferInventory,
    candidate: &KnowledgeTransferMatch,
    project_id: &str,
    operation_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<(String, TransferDestination), PluginError> {
    let key = &candidate.copy.artifact_id;
    let destination = if completed_copy(inventory, candidate) {
        completed_destination(&inventory.existing[key])
    } else {
        new_destination(
            context,
            &request.project_root,
            project_id,
            operation_id,
            key,
            inventory.existing.get(key),
        )?
    };
    Ok((key.clone(), destination))
}

fn new_destination(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    operation_id: &str,
    review_id: &str,
    existing: Option<&KnowledgeArtifact>,
) -> Result<TransferDestination, PluginError> {
    let expected_current = existing.map(|artifact| artifact.artifact_id.clone());
    let incoming_id = expected_current.clone().unwrap_or_else(|| review_id.into());
    let exact_id = destination_revision(context, root, project_id, operation_id, &incoming_id)?;
    Ok(TransferDestination {
        incoming_id,
        exact_id,
        expected_current,
    })
}

fn completed_destination(existing: &KnowledgeArtifact) -> TransferDestination {
    let exact_id = existing.metadata["inheritance"]["copied_revision_id"]
        .as_str()
        .unwrap_or(&existing.artifact_id)
        .to_owned();
    TransferDestination {
        incoming_id: existing.artifact_id.clone(),
        exact_id,
        expected_current: Some(existing.artifact_id.clone()),
    }
}

fn destination_revision(
    context: &mut PluginContext<'_>,
    root: &str,
    project_id: &str,
    operation_id: &str,
    incoming_id: &str,
) -> Result<String, PluginError> {
    if !has_destination_history(context, root, incoming_id)? {
        return Ok(incoming_id.to_owned());
    }
    lumvise_semantic_revisions::semantic_revision_successor_id(
        project_id,
        "local",
        operation_id,
        &SemanticRecordRef::artifact(incoming_id),
    )
    .map_err(|error| invalid(&json!(incoming_id), &error.to_string()))
}

fn has_destination_history(
    context: &mut PluginContext<'_>,
    root: &str,
    incoming_id: &str,
) -> Result<bool, PluginError> {
    let revision =
        crate::storage::read_revision(context, root, SemanticRecordRef::artifact(incoming_id))?;
    Ok(revision.is_some())
}

fn capture_scope(
    request: &ApplyRequest,
    inventory: &TransferInventory,
    selected: &[&KnowledgeTransferMatch],
    destinations: &HashMap<String, TransferDestination>,
    context: &mut PluginContext<'_>,
) -> Result<SemanticIndexObservation, PluginError> {
    let scope = selected
        .iter()
        .filter(|candidate| !completed_copy(inventory, candidate))
        .map(|candidate| &destinations[&candidate.copy.artifact_id])
        .collect::<Vec<_>>();
    let ids = scope
        .iter()
        .map(|destination| destination.incoming_id.clone())
        .collect::<Vec<_>>();
    let observed = crate::publication_observation::read(
        context,
        &request.project_root,
        json!({"kind":"none"}),
        &ids,
    )?;
    require_original_membership(&observed, &scope)?;
    Ok(observed)
}

fn require_original_membership(
    observed: &SemanticIndexObservation,
    scope: &[&TransferDestination],
) -> Result<(), PluginError> {
    let expected = scope
        .iter()
        .filter_map(|destination| destination.expected_current.as_ref())
        .map(SemanticRecordRef::artifact)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if observed.expected.records != expected {
        return Err(invalid(
            &json!(observed.expected.records),
            "the exact original destination copy membership",
        ));
    }
    Ok(())
}

fn prepare_copy(
    candidate: &KnowledgeTransferMatch,
    references: &references::TransferReferences,
    context: &mut PluginContext<'_>,
) -> Result<KnowledgeArtifact, PluginError> {
    let mut copy = references.prepare(candidate)?;
    copy.metadata["inheritance"]["transfer_id"] = json!(candidate.copy.artifact_id);
    copy.metadata["inheritance"]["copied_revision_id"] = json!(copy.artifact_id);
    crate::artifact::stamp(&mut copy, None, chrono::Utc::now());
    attachments::copy_referenced_attachments(&candidate.source, &mut copy, context)?;
    Ok(copy)
}

fn prepare_write(
    candidate: &KnowledgeTransferMatch,
    references: &references::TransferReferences,
    incoming_id: &str,
    context: &mut PluginContext<'_>,
) -> Result<(String, Value), PluginError> {
    let copy = prepare_copy(candidate, references, context)?;
    let mut artifact = storage::graph_artifact(&copy)?;
    artifact["artifact_id"] = json!(incoming_id);
    Ok((
        incoming_id.to_owned(),
        json!({"artifact":artifact, "media_type":"text/markdown; charset=utf-8", "checkpoint":true}),
    ))
}

pub(super) fn response(root: &str, receipt: Option<&Value>) -> Result<Value, PluginError> {
    let receipt = receipt.ok_or_else(|| invalid(&Value::Null, "an accepted transfer receipt"))?;
    let completion: TransferCompletion = crate::parse(
        receipt["transfer"].clone(),
        "frozen complete transfer result",
    )?;
    let actual = completion
        .copied
        .iter()
        .map(|id| {
            crate::artifact_publication::accepted_artifact_id(id, Some(receipt)).map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"project_root":root, "copied_artifact_ids":actual,
        "already_copied_artifact_ids":completion.already, "skipped_count":completion.skipped_count}))
}

fn invalid(value: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_transfer_publication",
        format!("transfer value `{value}`; expected {expected}"),
        false,
    )
}
