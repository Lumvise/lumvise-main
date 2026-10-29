//! Source-owned citation claims use ordinary pinned selection policy and durable receipts.
//! Adapters call the public planners with their authenticated actor, never a request field.
use crate::revision_index::digest;
use crate::validation::require_non_empty;
use crate::*;

pub use lumvise_contracts::{
    SemanticCitationRetentionAcquire, SemanticCitationRetentionIntent,
    SemanticCitationRetentionRelease,
};

const CITATION_NAMESPACE: &str = "knowledge-citation:";

/// Opaque row identities for scoped adapter fact loading; they reveal no private document IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticCitationRetentionKeys {
    pub selection_id: String,
    pub acquisition_id: String,
    pub release_id: String,
}

/// Derives source-owned identities using an adapter-authenticated actor.
/// Example: `citation_retention_keys(lineage, authenticated_subject, &intent)?`.
pub fn citation_retention_keys(
    project_id: &str,
    actor: &str,
    intent: &SemanticCitationRetentionIntent,
) -> SemanticRevisionResult<SemanticCitationRetentionKeys> {
    for value in [
        project_id,
        actor,
        &intent.space_id,
        &intent.document_id,
        &intent.publication_id,
    ] {
        require_non_empty(
            value,
            "non-empty source lineage, authenticated actor and citing intent fields",
        )?;
    }
    let key = digest(&("citation-retention-v1", project_id, actor, intent))?;
    Ok(SemanticCitationRetentionKeys {
        selection_id: format!("{CITATION_NAMESPACE}{key}"),
        acquisition_id: format!("{CITATION_NAMESPACE}acquire:{key}"),
        release_id: format!("{CITATION_NAMESPACE}release:{key}"),
    })
}

impl SemanticRevisionState {
    /// Retains one citation's exact evidence without exposing arbitrary pin mutation.
    /// Example: `state.plan_citation_acquisition(&request, verified_actor, &now, commit)?`.
    pub fn plan_citation_acquisition(
        &self,
        request: &SemanticCitationRetentionAcquire,
        actor: &str,
        timestamp: &SemanticRevisionTimestamp,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
        let keys = citation_retention_keys(&request.project_id, actor, &request.intent)?;
        require_acquirable(self, request, &keys)?;
        let update = acquisition_update(request, keys);
        self.plan_owned_selection(&update, timestamp, commit_version)
    }

    /// Releases only the original claim, requiring its original accepted acquisition.
    /// Example: `state.plan_citation_release(&request, verified_actor, &now, commit)?`.
    pub fn plan_citation_release(
        &self,
        request: &SemanticCitationRetentionRelease,
        actor: &str,
        timestamp: &SemanticRevisionTimestamp,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
        let keys = citation_retention_keys(&request.project_id, actor, &request.intent)?;
        let acquired = self.citation_acquisition(&keys)?;
        let update = release_update(request, keys, acquired)?;
        self.plan_owned_selection(&update, timestamp, commit_version)
    }

    fn citation_acquisition(
        &self,
        keys: &SemanticCitationRetentionKeys,
    ) -> SemanticRevisionResult<&SemanticRevisionPublicationReceipt> {
        self.accepted_operations
            .iter()
            .find(|operation| {
                operation.receipt.operation_id == keys.acquisition_id
                    && operation.receipt.selection.selection_id == keys.selection_id
                    && operation.receipt.selection.generation == 1
                    && !operation.receipt.accepted_scope.records.is_empty()
            })
            .map(|operation| &operation.receipt)
            .ok_or_else(|| {
                SemanticRevisionError::invalid_value(
                    &keys.selection_id,
                    "an acquired citation owned by the authenticated actor",
                )
            })
    }
}

pub(crate) fn require_general_selection(selection_id: &str) -> SemanticRevisionResult<()> {
    if selection_id.starts_with(CITATION_NAMESPACE)
        || selection_id.starts_with("challenge-contribution:")
    {
        return Err(SemanticRevisionError::invalid_value(
            selection_id,
            "a general identity outside source-owned citation and contribution namespaces",
        ));
    }
    Ok(())
}

fn require_acquirable(
    state: &SemanticRevisionState,
    request: &SemanticCitationRetentionAcquire,
    keys: &SemanticCitationRetentionKeys,
) -> SemanticRevisionResult<()> {
    if request.records.is_empty() {
        return Err(SemanticRevisionError::invalid_value(
            "empty citation evidence",
            "at least one exact record",
        ));
    }
    if state
        .accepted_operations
        .iter()
        .any(|operation| operation.receipt.operation_id == keys.release_id)
        || state
            .selection(&keys.selection_id)
            .is_some_and(|head| head.selection.generation > 1)
    {
        return Err(SemanticRevisionError::invalid_value(
            &keys.selection_id,
            "a new document publication intent after a citation was released",
        ));
    }
    Ok(())
}

fn acquisition_update(
    request: &SemanticCitationRetentionAcquire,
    keys: SemanticCitationRetentionKeys,
) -> SemanticRevisionSelectionUpdate {
    SemanticRevisionSelectionUpdate {
        operation_id: keys.acquisition_id,
        project_root: request.project_root.clone(),
        project_id: request.project_id.clone(),
        selection_id: keys.selection_id,
        source_context: request.source_context.clone(),
        expected_generation: 0,
        artifact_edits: request.artifact_edits.clone(),
        mode: SemanticSelectionMode::Pinned,
        records: request.records.clone(),
        connection_ids: vec![],
        source_anchor: None,
    }
}

fn release_update(
    request: &SemanticCitationRetentionRelease,
    keys: SemanticCitationRetentionKeys,
    acquired: &SemanticRevisionPublicationReceipt,
) -> SemanticRevisionResult<SemanticRevisionSelectionUpdate> {
    if request.expected_generation != acquired.selection.generation {
        return Err(SemanticRevisionError::invalid_value(
            request.expected_generation.to_string(),
            "the original citation acquisition generation",
        ));
    }
    Ok(SemanticRevisionSelectionUpdate {
        operation_id: keys.release_id,
        project_root: request.project_root.clone(),
        project_id: request.project_id.clone(),
        selection_id: keys.selection_id,
        source_context: acquired.selection.source_context.clone(),
        expected_generation: request.expected_generation,
        artifact_edits: Default::default(),
        mode: SemanticSelectionMode::Pinned,
        records: vec![],
        connection_ids: vec![],
        source_anchor: None,
    })
}
