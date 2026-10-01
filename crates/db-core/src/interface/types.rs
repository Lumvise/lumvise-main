pub use lumvise_contracts::ArtifactDependency;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use super::error::{DbError, Result};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeHookScope {
    pub project_root: String,
    #[serde(default)]
    pub entity_kinds: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeDisposition {
    Upserted,
    Removal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangedElement {
    pub element_id: String,
    pub entity_kind: String,
    pub revision: i64,
    pub disposition: ChangeDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeBatch {
    pub base_revision: i64,
    pub target_revision: i64,
    pub changed: Vec<ChangedElement>,
    #[serde(default)]
    pub(crate) hook_name: Option<String>,
    #[serde(default)]
    pub(crate) registration_generation: i64,
}

impl ChangeBatch {
    pub fn delivery_key(&self, changed: &ChangedElement) -> Result<String> {
        let hook_name = self.hook_name.as_deref().ok_or_else(|| {
            DbError::invalid_value("stateless batch", "batch returned by dirty_batch")
        })?;
        Ok(format!(
            "{hook_name}:{}:{}:{}:{:?}",
            self.registration_generation, changed.element_id, changed.revision, changed.disposition
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeHookRegistration {
    pub hook_name: String,
    pub scope: ChangeHookScope,
    pub watermark: i64,
    pub(crate) registration_generation: i64,
}

/// One registered MCP server instance row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpInstance {
    pub instance_id: String,
    pub project_root: String,
    pub display_name: String,
    pub status: String,
    pub capabilities_json: String,
    pub control_channel_json: Option<String>,
    pub last_heartbeat_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PzSnapshotResult {
    pub project_id: String,
    pub snapshot_id: String,
    pub commit_version: i64,
    pub published_at: String,
    pub output_path: PathBuf,
    pub output_bytes: u64,
    pub row_counts: BTreeMap<String, u64>,
}

/// Outcome of importing one PZ snapshot into a local project root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PzImportResult {
    /// Lineage identity recorded in the imported archive manifest.
    pub project_id: String,
    pub snapshot_id: String,
    /// Remote the archive was published from, when the manifest names one.
    pub canonical_remote: Option<String>,
    pub structure: SemanticBatchSyncReport,
    /// Archive artifacts inserted because their IDs were absent.
    pub artifacts_imported: usize,
    /// Archive artifacts skipped because the database already holds their IDs.
    pub artifacts_kept: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingRecord {
    pub scope: String,
    pub key: String,
    pub value: Value,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSettingsRecord {
    pub plugin_id: String,
    pub enabled: bool,
    pub config: Value,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDataTableRecord {
    pub plugin_id: String,
    pub table_name: String,
    pub schema: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDataRowRecord {
    pub plugin_id: String,
    pub table_name: String,
    pub row_key: String,
    pub value: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// One deterministic page of rows from a plugin-owned logical table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDataRowsPage {
    /// Rows ordered by ascending key.
    pub rows: Vec<PluginDataRowRecord>,
    /// Exclusive cursor for the next page, or `None` after the final page.
    pub next_after_key: Option<String>,
}

/// One namespaced row change in an atomic plugin-storage batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginDataMutation {
    /// Insert or replace one row.
    Put {
        /// Existing logical table owned by the invoking plugin.
        table_name: String,
        /// Unique row key within the logical table.
        row_key: String,
        /// JSON value stored for the row.
        value: Value,
    },
    /// Delete one row when present.
    Delete {
        /// Existing logical table owned by the invoking plugin.
        table_name: String,
        /// Unique row key within the logical table.
        row_key: String,
    },
}

/// Observable outcome of one committed plugin-storage mutation batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDataMutationResult {
    /// Number of put operations committed.
    pub rows_put: usize,
    /// Number of existing rows deleted.
    pub rows_deleted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticRecoveryStatus {
    pub commit_version: i64,
    pub state: String,
    pub failure_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBlob {
    pub content_ref: String,
    pub artifact_id: String,
    pub media_type: String,
    pub content: Vec<u8>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactTextVector {
    pub engine_id: String,
    pub model: Option<String>,
    pub dimensions: usize,
    pub vector: Vec<f32>,
    pub normalized: bool,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredArtifactTextVector {
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub source_text: String,
    pub vector: ArtifactTextVector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredSemanticElementNameVector {
    pub semantic_element_id: String,
    pub project_root: String,
    pub source_text: String,
    pub vector: ArtifactTextVector,
}

/// A nearest-neighbor result identified by the persisted semantic record ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub id: String,
    pub score: f32,
}

pub trait ArtifactTextVectorizer {
    fn vectorize_artifact_text(&self, text: &str) -> crate::Result<ArtifactTextVector>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticElement {
    pub project_root: String,
    pub semantic_element_id: String,
    pub semantic_source_id: String,
    pub path: String,
    pub element_kind: String,
    pub name: String,
    pub parent_element_id: Option<String>,
    pub content_fingerprint: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub lifecycle: String,
    #[serde(default)]
    pub match_evidence: Option<SemanticMatchEvidence>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticMatchEvidence {
    pub match_confidence: u8,
    pub simhash_distance: Option<u32>,
    pub matched_at: String,
    pub precaution: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticArtifact {
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub artifact_kind: String,
    pub title: String,
    pub content_ref: Option<String>,
    pub content: Option<String>,
    pub searchable_text: Option<String>,
    pub content_size_bytes: Option<usize>,
    #[serde(default)]
    pub dependencies: Vec<ArtifactDependency>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticRelationship {
    pub project_root: String,
    pub source_element_id: String,
    pub target_element_id: String,
    pub relationship_kind: String,
    pub label: String,
    pub metadata: Value,
}

/// Durable UUID v4 identity for one semantic project lineage.
///
/// The local source root is an input to identity lookup, but is intentionally
/// not part of this snapshot-facing value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectIdentity {
    pub project_id: String,
}

/// One owned, consistent project graph extracted through a DB Core snapshot lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticProjectSnapshot {
    pub commit_version: i64,
    pub published_at: String,
    pub project_root: String,
    pub elements: Vec<SemanticElement>,
    pub relationships: Vec<SemanticRelationship>,
    pub artifacts: Vec<SemanticArtifact>,
}

/// One stable, selective semantic subgraph read for a target element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticSelectiveSubgraph {
    pub commit_version: i64,
    pub published_at: String,
    pub project_root: String,
    pub root_element_id: String,
    pub elements: Vec<SemanticElement>,
    pub relationships: Vec<SemanticRelationship>,
    pub artifacts: Vec<SemanticArtifact>,
    #[serde(default)]
    pub external_elements: Vec<SemanticElement>,
}
/// Granularity used by the complete renderer graph projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticGraphGranularity {
    File,
    Class,
    Function,
    Method,
    Property,
}

/// Domain scope for one complete renderer graph request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticGraphProjectionRequest {
    pub project_root: String,
    pub target_path: Option<String>,
    pub granularity: SemanticGraphGranularity,
    pub recursive: bool,
    pub include_external: bool,
    pub include_first_neighbors: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticGraphArtifactPreview {
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(rename = "artifactKind")]
    pub artifact_kind: String,
    pub title: String,
    pub text: Option<String>,
    #[serde(rename = "contentRef")]
    pub content_ref: Option<String>,
    #[serde(rename = "contentSizeBytes")]
    pub content_size_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticGraphNode {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    #[serde(rename = "parentLabel")]
    pub parent_label: Option<String>,
    pub depth: usize,
    #[serde(rename = "isContainer")]
    pub is_container: bool,
    pub path: String,
    #[serde(rename = "lineStart")]
    pub line_start: Option<i64>,
    #[serde(rename = "lineEnd")]
    pub line_end: Option<i64>,
    #[serde(rename = "codeSize")]
    pub code_size: usize,
    #[serde(rename = "connectionStrength")]
    pub connection_strength: u64,
    #[serde(rename = "semanticArtifactCount")]
    pub semantic_artifact_count: usize,
    #[serde(rename = "commentCount")]
    pub comment_count: usize,
    #[serde(rename = "dataSemanticArtifactCount")]
    pub data_semantic_artifact_count: usize,
    pub summary: String,
    pub artifacts: Vec<SemanticGraphArtifactPreview>,
    #[serde(rename = "stableRef")]
    pub stable_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticGraphEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relationship_label: String,
    pub relationship_kind: String,
    #[serde(rename = "linkType")]
    pub link_type: String,
    pub weight: u64,
    pub samples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticGraphProjection {
    #[serde(rename = "commitVersion")]
    pub commit_version: i64,
    #[serde(rename = "publishedAt")]
    pub published_at: String,
    #[serde(rename = "projectRoot")]
    pub project_root: String,
    pub nodes: Vec<SemanticGraphNode>,
    pub edges: Vec<SemanticGraphEdge>,
}

/// Columnar desktop transport for one semantic graph projection.
///
/// Nodes retain their canonical domain shape. Edge endpoints use dense node
/// indexes, while edge IDs and link types are reconstructed by the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactSemanticGraphProjection {
    pub commit_version: i64,
    pub published_at: String,
    pub project_root: String,
    pub nodes: Vec<SemanticGraphNode>,
    pub edge_source_indices: Vec<u32>,
    pub edge_target_indices: Vec<u32>,
    pub edge_relationship_labels: Vec<String>,
    pub edge_relationship_kinds: Vec<String>,
    pub edge_weights: Vec<u64>,
    pub edge_samples: Vec<Vec<String>>,
}

impl SemanticGraphProjection {
    /// Compacts derivable edge fields for the desktop/Tauri transport.
    ///
    /// # Example
    ///
    /// ```
    /// let projection = lumvise_db_core::SemanticGraphProjection {
    ///     commit_version: 0,
    ///     published_at: "now".into(),
    ///     project_root: "/repo".into(),
    ///     nodes: vec![],
    ///     edges: vec![],
    /// };
    /// assert!(projection.into_compact().unwrap().nodes.is_empty());
    /// ```
    pub fn into_compact(self) -> crate::Result<CompactSemanticGraphProjection> {
        let node_indices = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                u32::try_from(index)
                    .map(|index| (node.id.as_str(), index))
                    .map_err(|_| {
                        crate::DbError::invalid_value(
                            self.nodes.len().to_string(),
                            "semantic graph node count fitting u32",
                        )
                    })
            })
            .collect::<crate::Result<HashMap<_, _>>>()?;
        let mut edge_source_indices = Vec::with_capacity(self.edges.len());
        let mut edge_target_indices = Vec::with_capacity(self.edges.len());
        let mut edge_relationship_labels = Vec::with_capacity(self.edges.len());
        let mut edge_relationship_kinds = Vec::with_capacity(self.edges.len());
        let mut edge_weights = Vec::with_capacity(self.edges.len());
        let mut edge_samples = Vec::with_capacity(self.edges.len());
        for edge in self.edges {
            let source = node_indices
                .get(edge.source.as_str())
                .copied()
                .ok_or_else(|| {
                    crate::DbError::invalid_value(
                        format!("edge `{}` source `{}`", edge.id, edge.source),
                        "edge source naming a projected node",
                    )
                })?;
            let target = node_indices
                .get(edge.target.as_str())
                .copied()
                .ok_or_else(|| {
                    crate::DbError::invalid_value(
                        format!("edge `{}` target `{}`", edge.id, edge.target),
                        "edge target naming a projected node",
                    )
                })?;
            edge_source_indices.push(source);
            edge_target_indices.push(target);
            edge_relationship_labels.push(edge.relationship_label);
            edge_relationship_kinds.push(edge.relationship_kind);
            edge_weights.push(edge.weight);
            edge_samples.push(edge.samples);
        }
        Ok(CompactSemanticGraphProjection {
            commit_version: self.commit_version,
            published_at: self.published_at,
            project_root: self.project_root,
            nodes: self.nodes,
            edge_source_indices,
            edge_target_indices,
            edge_relationship_labels,
            edge_relationship_kinds,
            edge_weights,
            edge_samples,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticBatchSyncReport {
    pub elements_upserted: usize,
    pub relationships_upserted: usize,
    pub identities_reused: usize,
    pub elements_marked_inactive: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPartition {
    pub project_root: String,
    pub replace_paths: Vec<String>,
}

/// Desired durable registration for one ready compiled background export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginBackgroundRegistrationSpec {
    pub plugin_id: String,
    pub export_id: String,
    pub export_kind: String,
    pub contract: Value,
    pub next_due_at: Option<i64>,
}

/// Durable registration synchronized from the ready signed plugin catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginBackgroundRegistration {
    pub plugin_id: String,
    pub export_id: String,
    pub export_kind: String,
    pub contract: Value,
    pub active: bool,
    pub next_due_at: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable idempotent delivery for one compiled background export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginBackgroundDelivery {
    pub delivery_id: String,
    pub plugin_id: String,
    pub export_id: String,
    pub delivery_kind: String,
    pub source_key: String,
    pub payload: Value,
    pub state: String,
    pub attempts: u32,
    pub next_attempt_at: i64,
    pub lease_expires_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_projection_replaces_edge_identifiers_with_dense_endpoints() {
        let projection = SemanticGraphProjection {
            commit_version: 7,
            published_at: "2026-07-26T00:00:00Z".into(),
            project_root: "/repo".into(),
            nodes: vec![graph_node("source"), graph_node("target")],
            edges: vec![SemanticGraphEdge {
                id: "source|semantic|calls|target".into(),
                source: "source".into(),
                target: "target".into(),
                relationship_label: "calls".into(),
                relationship_kind: "semantic".into(),
                link_type: "calls".into(),
                weight: 4,
                samples: vec!["sample".into()],
            }],
        };

        let compact = projection.into_compact().unwrap();
        assert_eq!(compact.edge_source_indices, vec![0]);
        assert_eq!(compact.edge_target_indices, vec![1]);
        assert_eq!(compact.edge_relationship_labels, vec!["calls"]);
        assert_eq!(compact.edge_relationship_kinds, vec!["semantic"]);
        assert_eq!(compact.edge_weights, vec![4]);
        assert_eq!(compact.edge_samples, vec![vec!["sample".to_string()]]);
    }

    fn graph_node(id: &str) -> SemanticGraphNode {
        SemanticGraphNode {
            id: id.into(),
            label: id.into(),
            kind: "file".into(),
            parent_id: None,
            parent_label: None,
            depth: 0,
            is_container: false,
            path: format!("src/{id}.rs"),
            line_start: Some(1),
            line_end: Some(2),
            code_size: 2,
            connection_strength: 4,
            semantic_artifact_count: 0,
            comment_count: 0,
            data_semantic_artifact_count: 0,
            summary: format!("file src/{id}.rs"),
            artifacts: Vec::new(),
            stable_ref: id.into(),
        }
    }
}
