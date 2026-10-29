//! Portable publication input and accepted deltas. Adapters own durable transactions.
use crate::*;
use serde::{Deserialize, Serialize};

/// One stored body, or its explicit tombstone after safe collection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticStoredRevision {
    pub record: SemanticRecordRef,
    pub element_scope: Option<SemanticRevisionElementScope>,
    pub body: SemanticRevisionBody,
    pub artifact_state: Option<SemanticArtifactRevisionState>,
    pub created_at_ms: i64,
    pub unselected_at_ms: Option<i64>,
    pub pruned_at_ms: Option<i64>,
}

/// Working edits advance this counter; freezing preserves its exact body under the same ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticArtifactRevisionState {
    pub edit_version: u64,
    pub frozen: bool,
}

/// Tombstones retain enough ownership information to reject stale scoped writes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionElementScope {
    pub path: String,
    pub provider_instance_id: Option<String>,
}

/// The current IDs of a selection; record bodies are stored only once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionSelectionState {
    pub selection: SemanticRevisionSelection,
    pub records: Vec<SemanticRecordRef>,
    pub connection_ids: Vec<String>,
}

/// A contextual artifact attachment. Explicit evidence dependencies stay on the artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionAssociation {
    pub association_id: String,
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub source_context: String,
    pub created_at_ms: i64,
    pub inherited: bool,
}

/// Durable operation identity prevents a lost acknowledgement from minting another revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionAcceptedOperation {
    pub request_root: String,
    pub request_digest: String,
    pub receipt: SemanticRevisionPublicationReceipt,
}

/// An adapter's consistent project view. No transport, clock or database handle enters policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionState {
    pub project_root: String,
    /// Existing durable lineage identity; checkout roots are routing aliases.
    pub project_id: String,
    pub commit_version: i64,
    pub records: Vec<SemanticStoredRevision>,
    pub connections: Vec<SemanticRevisionConnection>,
    pub selections: Vec<SemanticRevisionSelectionState>,
    pub selection_history: Vec<SemanticRevisionSelectionDelta>,
    pub revision_links: Vec<SemanticRevisionLink>,
    pub associations: Vec<SemanticRevisionAssociation>,
    pub accepted_operations: Vec<SemanticRevisionAcceptedOperation>,
    pub retention_operations: Vec<SemanticRevisionAcceptedRetention>,
}

/// A stale writer receives the actual head and the exact conflicting scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionConflict {
    pub project_root: String,
    pub expected_project_id: String,
    pub actual_project_id: String,
    pub selection_id: String,
    pub expected_generation: i64,
    pub actual_generation: i64,
    pub records: Vec<SemanticRecordRef>,
    pub connection_ids: Vec<String>,
}

/// Conflicts are portable domain results rather than flattened transport errors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "value", rename_all = "snake_case")]
pub enum SemanticRevisionPublicationOutcome {
    Accepted(SemanticRevisionPublicationReceipt),
    Conflict(SemanticRevisionConflict),
}

/// The only rows an adapter needs to publish atomically with its existing commit boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionPublicationPlan {
    pub expected_commit_version: i64,
    pub outcome: SemanticRevisionPublicationOutcome,
    pub records: Vec<SemanticStoredRevision>,
    pub connections: Vec<SemanticRevisionConnection>,
    pub selections: Vec<SemanticRevisionSelectionState>,
    pub selection_deltas: Vec<SemanticRevisionSelectionDelta>,
    pub revision_links: Vec<SemanticRevisionLink>,
    pub associations: Vec<SemanticRevisionAssociation>,
    pub accepted_operation: Option<SemanticRevisionAcceptedOperation>,
}

/// Time supplied by the existing adapter clock, not a second clock inside policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionTimestamp {
    pub unix_ms: i64,
    pub rfc3339: String,
}

impl SemanticRevisionSelectionState {
    /// Supplies original body tokens with scoped membership. Example: `head.expectation(&state.records)`.
    pub fn expectation(&self, records: &[SemanticStoredRevision]) -> SemanticSelectionExpectation {
        SemanticSelectionExpectation {
            generation: self.selection.generation,
            records: self.records.clone(),
            connection_ids: self.connection_ids.clone(),
            artifact_edits: crate::artifact_working::edit_versions(&self.records, records),
        }
    }
}

/// Durable sweep acknowledgement; repeating the same request never collects a newer graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionAcceptedRetention {
    pub request_root: String,
    pub request_digest: String,
    pub receipt: SemanticRevisionRetentionReceipt,
}
