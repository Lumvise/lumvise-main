//! Portable revision publication, selection and retention records.
//! Callers use crate-root exports; storage plans and backend handles remain private.

use serde::{Deserialize, Serialize};

use super::{SemanticArtifact, SemanticElement, SemanticPartition, SemanticRelationship};

pub use lumvise_contracts::{
    SemanticIndexObservation, SemanticProducerBase, SemanticRecordKind, SemanticRecordRef,
    SemanticRevisionFamilyQuery, SemanticRevisionRemap, SemanticSelectionExpectation,
};

/// Accepted ancestry in one source context; navigation alone does not retain bodies.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionLink {
    pub project_root: String,
    pub source_context: String,
    pub older: SemanticRecordRef,
    pub newer: SemanticRecordRef,
    pub created_at_ms: i64,
    pub commit_version: i64,
}

/// Whether indexing may advance a selection or the selected IDs are explicitly pinned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSelectionMode {
    FollowAccepted,
    Pinned,
}

/// The source represented by a publication; a dirty checkout cannot claim a clean anchor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceAnchor {
    pub revision: String,
    pub canonical_remote: Option<String>,
    pub dirty: bool,
}

/// One project/source selection. Its generation is a consistency token, not a graph copy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionSelection {
    pub project_root: String,
    pub selection_id: String,
    pub source_context: String,
    pub mode: SemanticSelectionMode,
    pub generation: i64,
    pub source_anchor: Option<SemanticSourceAnchor>,
}

/// A selected connection identity, including duplicate occurrences without revision families.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionConnection {
    pub connection_id: String,
    pub relationship: SemanticRelationship,
}

/// A consistent selection read, with the expectation needed for conditional publication.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionSelectionSnapshot {
    pub selection: SemanticRevisionSelection,
    pub expectation: SemanticSelectionExpectation,
    pub commit_version: i64,
    pub published_at: String,
    pub elements: Vec<SemanticElement>,
    pub artifacts: Vec<SemanticArtifact>,
    pub connections: Vec<SemanticRevisionConnection>,
    /// Exact dependency bodies outside primary membership; these are not additional selected records.
    pub required_records: Vec<crate::SemanticStoredRevision>,
    /// Contextual attachments in this selection's source context, including required artifacts.
    pub associations: Vec<crate::SemanticRevisionAssociation>,
}

/// Scoped membership and original working-body tokens, without hydrating a snapshot for callers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionSelectionObservation {
    pub selection: SemanticRevisionSelection,
    pub expected: SemanticSelectionExpectation,
}

/// An authoritative structure edit within the existing provider/path ownership boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionStructure {
    pub partition: Option<SemanticPartition>,
    pub elements: Vec<SemanticElement>,
    pub relationships: Vec<SemanticRelationship>,
}

/// An artifact edit based on the supplied artifact ID; accepted IDs come from the receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticArtifactRevisionWrite {
    pub artifact: SemanticArtifact,
    pub media_type: String,
    /// Preserves replaced and accepted states for a reviewed snapshot; ordinary saves stay working.
    #[serde(default)]
    pub checkpoint: bool,
}

/// One idempotent, conditional semantic publication. Unmentioned artifacts remain authoritative.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionPublication {
    pub operation_id: String,
    pub project_root: String,
    /// Durable lineage observed before producing this edit; a route cannot substitute another project.
    pub project_id: String,
    pub selection_id: String,
    pub source_context: String,
    /// Advances this checkout's pinned base atomically, preserving its unmentioned scopes.
    pub producer_base: Option<SemanticProducerBase>,
    pub expected: SemanticSelectionExpectation,
    pub structure: Option<SemanticRevisionStructure>,
    pub artifacts: Vec<SemanticArtifactRevisionWrite>,
    pub removed_artifact_ids: Vec<String>,
    pub source_anchor: Option<SemanticSourceAnchor>,
}

/// The durable result returned again after an acknowledgement is lost.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionPublicationReceipt {
    pub operation_id: String,
    pub selection: SemanticRevisionSelection,
    /// The producer pin committed with this receipt, independently of the shared head generation.
    pub producer_base: Option<SemanticRevisionSelection>,
    /// Accepted membership only for this operation's authoritative scope, never unrelated head changes.
    pub accepted_scope: SemanticSelectionExpectation,
    pub commit_version: i64,
    pub remaps: Vec<SemanticRevisionRemap>,
    pub revision_links: Vec<SemanticRevisionLink>,
}

/// Explicitly selects existing revisions, including acknowledged historical or public pins.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionSelectionUpdate {
    pub operation_id: String,
    pub project_root: String,
    pub project_id: String,
    pub selection_id: String,
    pub source_context: String,
    pub expected_generation: i64,
    /// Original tokens for working artifacts included in an explicit selection or pin.
    #[serde(default)]
    pub artifact_edits: std::collections::BTreeMap<String, u64>,
    pub mode: SemanticSelectionMode,
    pub records: Vec<SemanticRecordRef>,
    pub connection_ids: Vec<String>,
    pub source_anchor: Option<SemanticSourceAnchor>,
}

/// The exact historical payload, or an explicit absence after safe collection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "record", rename_all = "snake_case")]
pub enum SemanticRevisionBody {
    Element(SemanticElement),
    Artifact(SemanticArtifact),
    Pruned,
}

/// Resolves an old ID without substituting a different record's body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionResolution {
    pub record: SemanticRecordRef,
    pub element_scope: Option<crate::SemanticRevisionElementScope>,
    pub body: SemanticRevisionBody,
    pub artifact_state: Option<crate::SemanticArtifactRevisionState>,
    pub created_at_ms: i64,
    pub pruned_at_ms: Option<i64>,
    pub successors: Vec<SemanticRevisionLink>,
}

/// Retention eligibility; selected and required content remains protected in either mode.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticRevisionRetentionPolicy {
    Age { history_age_seconds: u64 },
    RevisionCount { revisions: u32 },
}

impl Default for SemanticRevisionRetentionPolicy {
    /// Starts with thirty days. Example: `SemanticRevisionRetentionPolicy::default()`.
    fn default() -> Self {
        Self::Age {
            history_age_seconds: 30 * 24 * 60 * 60,
        }
    }
}

/// A collection proposal fenced against concurrent publication or a new pin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionRetentionPlan {
    pub operation_id: String,
    pub project_root: String,
    pub project_id: String,
    pub expected_commit_version: i64,
    pub evaluated_at_ms: i64,
    pub records: Vec<SemanticRecordRef>,
    pub expired_association_ids: Vec<String>,
}

/// Records and owned bytes actually collected by the fenced plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionRetentionReceipt {
    pub operation_id: String,
    pub commit_version: i64,
    pub pruned_records: Vec<SemanticRecordRef>,
    pub removed_blob_refs: Vec<String>,
    pub removed_associations: usize,
}
