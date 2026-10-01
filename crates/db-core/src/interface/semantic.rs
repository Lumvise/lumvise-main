use std::collections::HashSet;

use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactBlob, ChangeBatch, ChangeHookRegistration, ChangeHookScope, ChangedElement, DbError,
    ProjectIdentity, SemanticArtifact, SemanticBatchSyncReport, SemanticElement,
    SemanticGraphProjection, SemanticGraphProjectionRequest, SemanticPartition,
    SemanticProjectSnapshot, SemanticRelationship, SemanticSelectiveSubgraph,
    StoredArtifactTextVector, StoredSemanticElementNameVector, VectorSearchResult,
};

pub type PersistenceResult<T> = std::result::Result<T, DbError>;

/// Owned, engine-neutral semantic persistence requests.
/// Exclusive selector for one complete published semantic project snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSnapshotScope {
    ProjectRoot(String),
    SemanticElement(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SemanticOperation {
    /// Creates and atomically publishes one complete validated PZ snapshot.
    CreatePzSnapshot {
        project_root: String,
        output_path: String,
    },
    SyncStructure {
        project_root: String,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    BeginProjectSnapshot {
        snapshot_id: String,
        project_root: String,
        partition_paths: Vec<String>,
        page_count: usize,
    },
    StageProjectSnapshot {
        snapshot_id: String,
        page_index: usize,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    CommitProjectSnapshot {
        snapshot_id: String,
    },
    AbortProjectSnapshot {
        snapshot_id: String,
    },
    SyncPartition {
        partition: SemanticPartition,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    Element {
        semantic_element_id: String,
    },
    ElementsByIds {
        project_root: String,
        semantic_element_ids: HashSet<String>,
    },
    ElementsByIdsIncludingInactive {
        project_root: String,
        semantic_element_ids: HashSet<String>,
    },
    ArtifactsByIds {
        artifact_ids: HashSet<String>,
    },
    SearchElementCandidates {
        project_root: Option<String>,
        query: String,
        limit: usize,
    },
    SearchElementNameVectors {
        project_root: String,
        query: Vec<f32>,
        k: usize,
        engine_id: String,
        model: Option<String>,
    },
    StoreElementNameVectors {
        project_root: String,
        vectors: Vec<StoredSemanticElementNameVector>,
    },
    SearchArtifactTextVectors {
        project_root: String,
        query: Vec<f32>,
        k: usize,
        engine_id: String,
        model: Option<String>,
    },
    StoreArtifactTextVectors {
        project_root: String,
        vectors: Vec<StoredArtifactTextVector>,
    },
    RelationshipsFrom {
        semantic_element_id: String,
    },
    RelationshipsTouchingElements {
        semantic_element_ids: HashSet<String>,
    },
    Artifact {
        artifact_id: String,
    },
    ArtifactBlobGet {
        content_ref: String,
    },
    ArtifactBlobPut {
        content_ref: String,
        artifact_id: String,
        media_type: String,
        content: Vec<u8>,
    },
    ArtifactsForElements {
        semantic_element_ids: HashSet<String>,
    },
    ArtifactsForElementWithInheritance {
        semantic_element_id: String,
    },
    /// Lists artifacts that depend on one semantic element or artifact target.
    ArtifactDependents {
        target_kind: String,
        target_id: String,
    },
    UpsertArtifact {
        artifact: SemanticArtifact,
        media_type: String,
    },
    RemoveArtifact {
        artifact_id: String,
    },
    RemoveElement {
        semantic_element_id: String,
    },
    ProjectRoots,
    ProjectIdentity {
        project_root: String,
    },
    SemanticRevision,
    ProjectArtifacts {
        project_root: String,
        artifact_namespace: Option<String>,
    },
    ProjectSnapshot {
        scope: ProjectSnapshotScope,
        artifact_namespace: Option<String>,
    },
    SelectiveSubgraph {
        project_root: String,
        root_element_id: String,
        artifact_namespace: Option<String>,
    },
    ProjectRendererGraph(SemanticGraphProjectionRequest),
    ChangesSinceRevision {
        scope: ChangeHookScope,
        after_revision: i64,
        limit: usize,
    },
    RegisterChangeHook {
        hook_name: String,
        scope: ChangeHookScope,
    },
    ChangeHookRegistrations,
    DeregisterChangeHook {
        hook_name: String,
    },
    ChangeHookDirtyBatch {
        hook_name: String,
        maximum_elements: usize,
    },
    AcknowledgeChangeHook {
        batch: ChangeBatch,
    },
    WaitForSemanticRevision {
        after_revision: i64,
        timeout_ms: Option<u64>,
    },
    Maintenance,
    /// Bounded cross-project identity lookup: unions exact matches on raw
    /// content fingerprint, normalized `(kind, name)`, and normalized
    /// `(kind, file name)`. Authorized to `builtin.knowledge` only.
    CandidateSourceElements {
        content_fingerprints: HashSet<String>,
        kind_name_keys: HashSet<String>,
        kind_file_name_keys: HashSet<String>,
    },
    ScopedRead(crate::ScopedSemanticRead),
    /// Counts project elements by kind without returning their records.
    ProjectElementCounts {
        project_root: String,
    },
    /// Imports one validated PZ snapshot into `project_root`. The archive's
    /// structure replaces the project's published structure; artifacts absent
    /// from the database are inserted with their blobs and text vectors, while
    /// existing artifact IDs stay authoritative and are kept unchanged.
    ImportPzSnapshot {
        project_root: String,
        input_path: String,
    },
}

/// Stateless graph-derived delta page for transient external consumers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesSinceRevisionPage {
    pub base_revision: i64,
    pub target_revision: i64,
    /// Latest published revision when the page was read; a consumer starts
    /// live at this value without replaying history.
    pub head_revision: i64,
    pub has_more: bool,
    pub changed: Vec<ChangedElement>,
}

/// Every successful semantic operation returns the data it observed or wrote.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SemanticResult {
    SyncStructure(SemanticBatchSyncReport),
    SyncPartition(SemanticBatchSyncReport),
    Element(Option<SemanticElement>),
    Elements(Vec<SemanticElement>),
    ElementVectorSearch(Vec<VectorSearchResult>),
    StoredElementNameVectors {
        count: usize,
    },
    ArtifactVectorSearch(Vec<VectorSearchResult>),
    StoredArtifactTextVectors {
        count: usize,
    },
    Relationships(Vec<SemanticRelationship>),
    Artifact(Option<SemanticArtifact>),
    Artifacts(Vec<SemanticArtifact>),
    UpsertedArtifact {
        artifact_id: String,
    },
    ArtifactBlob(Option<ArtifactBlob>),
    Removed {
        removed: bool,
    },
    ProjectRoots(Vec<String>),
    ProjectIdentity(ProjectIdentity),
    SemanticRevision {
        commit_version: i64,
    },
    ProjectSnapshot(SemanticProjectSnapshot),
    SelectiveSubgraph(Option<SemanticSelectiveSubgraph>),
    RendererGraphProjection(SemanticGraphProjection),
    ChangesSinceRevision(ChangesSinceRevisionPage),
    PzSnapshot(crate::PzSnapshotResult),
    ChangeHookDeregistered {
        removed: bool,
    },
    ChangeHookRegistration(ChangeHookRegistration),
    ChangeHookRegistrations(Vec<ChangeHookRegistration>),
    ChangeHookBatch(ChangeBatch),
    ChangeHookAcknowledged {
        advanced: bool,
    },
    MaintenanceCompleted,
    SnapshotStaging {
        snapshot_id: String,
        staged_pages: usize,
        page_count: usize,
    },
    SnapshotCommitted(SemanticBatchSyncReport),
    SnapshotAborted {
        snapshot_id: String,
        discarded_pages: usize,
    },
    ScopedGraph(crate::SemanticScopedGraph),
    ProjectElementCounts {
        commit_version: i64,
        published_at: String,
        total_elements: usize,
        elements_by_kind: std::collections::BTreeMap<String, usize>,
    },
    PzImport(crate::PzImportResult),
}

pub trait SemanticPersistence: Send + Sync {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> PersistenceResult<SemanticResult>;
    fn readiness(&self) -> PersistenceResult<SemanticReadiness>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticReadiness {
    pub ready: bool,
}
