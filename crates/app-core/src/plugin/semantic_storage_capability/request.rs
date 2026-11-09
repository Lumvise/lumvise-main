//! Closed process-boundary request schema; authorization and execution stay in the parent.
use super::{default_media_type, default_true};
use lumvise_db_core::{
    ProjectSnapshotScope, SemanticArtifact, SemanticElement, SemanticPartition,
    SemanticRelationship, StoredSemanticElementNameVector, StoredSemanticElementTextVector,
};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SemanticStorageRequest {
    ProjectIdentity {
        project_root: String,
    },
    RevisionSelection {
        project_root: String,
        selection_id: String,
    },
    InspectRevisionIntegrity {
        project_root: String,
        publication_operation_id: Option<String>,
    },
    RecoverRevisionIntegrity {
        request: lumvise_db_core::SemanticRevisionRecoveryRequest,
    },
    SelectRevisions {
        update: lumvise_db_core::SemanticRevisionSelectionUpdate,
    },
    PlanRevisionRetention {
        project_root: String,
        operation_id: String,
        policy: lumvise_db_core::SemanticRevisionRetentionPolicy,
    },
    CollectRevisionRetention {
        plan: lumvise_db_core::SemanticRevisionRetentionPlan,
        policy: lumvise_db_core::SemanticRevisionRetentionPolicy,
    },
    RevisionSelectionMembers {
        query: lumvise_db_core::SemanticRevisionSelectionQuery,
    },
    RevisionFamilies {
        query: lumvise_db_core::SemanticRevisionFamilyQuery,
    },
    ResolveRevision {
        project_root: String,
        record: lumvise_db_core::SemanticRecordRef,
    },
    PublishRevisions {
        publication: lumvise_db_core::SemanticRevisionPublication,
    },
    CopyArtifactAttachment {
        source_artifact_id: String,
        target_artifact_id: String,
        content_ref: String,
    },
    SyncStructure {
        project_root: String,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
    },
    BeginProjectSnapshot {
        snapshot_id: String,
        project_root: String,
        #[serde(default)]
        provider_instance_id: Option<String>,
        partition_paths: Vec<String>,
        page_count: usize,
        observation: lumvise_contracts::SemanticIndexObservation,
    },
    StageProjectSnapshot {
        snapshot_id: String,
        page_index: usize,
        elements: Vec<SemanticElement>,
        relationships: Vec<SemanticRelationship>,
        #[serde(default)]
        sources: Vec<lumvise_contracts::SemanticSourceUpsert>,
        #[serde(default)]
        artifacts: Vec<lumvise_db_core::SemanticArtifactRevisionWrite>,
        #[serde(default)]
        removed_artifact_ids: Vec<String>,
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
        #[serde(default)]
        model: Option<String>,
    },
    StoreElementNameVectors {
        project_root: String,
        vectors: Vec<StoredSemanticElementNameVector>,
    },
    SearchElementTextVectors {
        project_root: String,
        query: Vec<f32>,
        k: usize,
        engine_id: String,
        #[serde(default)]
        model: Option<String>,
    },
    StoreElementTextVectors {
        project_root: String,
        vectors: Vec<StoredSemanticElementTextVector>,
    },
    BeginElementTextVectorGeneration {
        spec: lumvise_db_core::ElementTextVectorGenerationSpec,
    },
    StageElementTextVectorChunk {
        key: lumvise_db_core::ElementTextVectorGenerationKey,
        chunk: u32,
        vector: Vec<f32>,
    },
    CommitElementTextVectorGeneration {
        key: lumvise_db_core::ElementTextVectorGenerationKey,
    },
    AbortElementTextVectorGeneration {
        key: lumvise_db_core::ElementTextVectorGenerationKey,
    },
    SearchArtifactTextVectors {
        project_root: String,
        query: Vec<f32>,
        k: usize,
        engine_id: String,
        #[serde(default)]
        model: Option<String>,
    },
    StoreArtifactTextVectors {
        project_root: String,
        vectors: Vec<lumvise_db_core::StoredArtifactTextVector>,
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
    ArtifactsForElements {
        semantic_element_ids: HashSet<String>,
    },
    ArtifactsForElementWithInheritance {
        semantic_element_id: String,
    },
    ArtifactDependents {
        target_kind: String,
        target_id: String,
    },
    UpsertArtifact {
        artifact: SemanticArtifact,
        #[serde(default = "default_media_type")]
        media_type: String,
    },
    RemoveArtifact {
        artifact_id: String,
    },
    RemoveElement {
        semantic_element_id: String,
    },
    ProjectArtifacts {
        project_root: String,
        artifact_namespace: Option<String>,
    },
    ProjectRoots,
    SemanticRevision,
    ProjectElementCounts {
        project_root: String,
    },
    ScopedRead {
        request: lumvise_db_core::ScopedSemanticRead,
    },
    ProjectSnapshot {
        scope: ProjectSnapshotScope,
        artifact_namespace: Option<String>,
    },
    SearchProjectStructure {
        project_root: String,
    },
    SelectiveSubgraph {
        project_root: String,
        root_element_id: String,
        artifact_namespace: Option<String>,
    },
    ProjectRendererGraph {
        project_root: String,
        target_path: Option<String>,
        granularity: lumvise_db_core::SemanticGraphGranularity,
        #[serde(default = "default_true")]
        recursive: bool,
        #[serde(default)]
        include_external: bool,
        #[serde(default)]
        include_first_neighbors: bool,
    },
    CandidateSourceElements {
        content_fingerprints: HashSet<String>,
        kind_name_keys: HashSet<String>,
        kind_file_name_keys: HashSet<String>,
    },
}
