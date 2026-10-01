use lumvise_contracts::SemanticElementV2;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const DEFAULT_TREE_DEPTH: usize = 64;
pub(crate) const DEFAULT_DEPENDENCY_DEPTH: usize = 3;

/// Source descriptor supplied by one semantic index provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SemanticSource {
    /// Stable source identity.
    pub semantic_source_id: String,
    /// Source kind such as `repository`.
    pub kind: String,
    /// Human-readable source name.
    pub name: String,
    /// Optional local source root.
    pub root_path: Option<String>,
    /// Canonical source URI.
    pub root_uri: String,
    /// Owning project root.
    #[serde(default)]
    pub project_root: String,
}

/// Neutral semantic graph element persisted by compiled Semantic.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SemanticElement {
    /// Project owning this element.
    pub project_root: String,
    /// Stable element identity.
    pub semantic_element_id: String,
    /// Source that produced this element.
    pub semantic_source_id: String,
    /// Project-relative path.
    pub path: String,
    /// Element kind.
    pub element_kind: String,
    /// Human-readable element name.
    pub name: String,
    /// Optional denormalized parent identity.
    pub parent_element_id: Option<String>,
    /// Optional source fingerprint.
    pub content_fingerprint: Option<String>,
    /// Inclusive start line.
    pub start_line: Option<i64>,
    /// Inclusive end line.
    pub end_line: Option<i64>,
    /// `active` or `inactive`.
    pub lifecycle: String,
    /// Provider-owned metadata.
    pub metadata: Value,
}

/// Neutral directed semantic relationship.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SemanticRelationship {
    /// Owning project.
    pub project_root: String,
    /// Source element.
    pub source_element_id: String,
    /// Target element.
    pub target_element_id: String,
    /// Machine-readable relationship kind.
    pub relationship_kind: String,
    /// Human-readable relationship label.
    pub label: String,
    /// `active` or `inactive`.
    #[serde(default = "active_lifecycle")]
    pub lifecycle: String,
    /// Provider-owned metadata.
    pub metadata: Value,
}

fn active_lifecycle() -> String {
    "active".into()
}

/// Semantic artifact included by an index provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SemanticArtifact {
    /// Project owning this artifact.
    #[serde(default)]
    pub project_root: String,
    /// Stable artifact identity.
    pub artifact_id: String,
    /// Element receiving this artifact.
    pub semantic_element_id: String,
    /// Artifact kind.
    pub artifact_kind: String,
    /// Human-readable title.
    pub title: String,
    /// Optional external content reference.
    pub content_ref: Option<String>,
    /// Optional inline content.
    pub content: Option<String>,
    /// Optional normalized search content.
    pub searchable_text: Option<String>,
    /// Declared content size.
    pub content_size_bytes: Option<usize>,
    /// Provider-owned metadata.
    pub metadata: Value,
}

impl SemanticElement {
    pub(crate) fn view(&self, parent_element_id: Option<String>) -> SemanticElementV2 {
        SemanticElementV2 {
            semantic_element_id: self.semantic_element_id.clone(),
            element_kind: self.element_kind.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            parent_element_id,
            start_line: self.start_line,
            end_line: self.end_line,
            project_root: None,
            semantic_source_id: None,
            content_fingerprint: None,
            lifecycle: None,
            metadata: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SemanticTreeRequest {
    pub project_root: Option<String>,
    pub semantic_element_id: Option<String>,
    pub max_depth: Option<usize>,
    pub include_inactive: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ElementAtLocationRequest {
    pub project_root: String,
    pub path: String,
    pub line: i64,
    pub element_kind: Option<String>,
    pub include_inactive: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RebuildSearchIndexRequest {
    pub project_root: String,
}
#[derive(Debug, Deserialize)]
pub(crate) struct IndexBatchRequest {
    pub provider_instance_id: String,
    pub project_root: String,
    #[serde(default)]
    pub ingestion_job_id: Option<String>,
    #[serde(default)]
    pub ingestion_page_index: Option<usize>,
    #[serde(default)]
    pub ingestion_page_count: Option<usize>,
    #[serde(default)]
    pub replace_paths: Vec<String>,
    #[serde(default)]
    pub removed_paths: Vec<String>,
    #[serde(default)]
    pub removed_artifact_ids: Vec<String>,
    pub semantic_sources: Vec<SemanticSourceInput>,
    pub semantic_elements: Vec<SemanticElementInput>,
    pub semantic_relationships: Vec<SemanticRelationshipInput>,
    #[serde(default)]
    pub semantic_artifacts: Vec<SemanticArtifactInput>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SemanticSourceInput {
    pub semantic_source_id: String,
    pub kind: String,
    pub name: String,
    pub root_path: Option<String>,
    pub root_uri: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SemanticElementInput {
    pub semantic_source_id: String,
    pub semantic_element_id: String,
    pub path: String,
    pub semantic_element_type: String,
    pub semantic_element_name: String,
    pub parent_element_id: Option<String>,
    pub content_fingerprint: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SemanticRelationshipInput {
    pub source_element_id: String,
    pub target_element_id: Option<String>,
    pub relationship_kind: String,
    pub relationship_label: String,
    pub target_label: Option<String>,
    pub target_locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SemanticArtifactInput {
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub artifact_kind: String,
    pub title: String,
    pub content_ref: Option<String>,
    pub content: Option<String>,
    pub searchable_text: Option<String>,
    pub content_size_bytes: Option<usize>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IndexLogRequest {
    pub provider_instance_id: String,
    pub semantic_source_id: Option<String>,
    pub plugin_id: Option<String>,
    pub project_root: Option<String>,
    pub index_log_id: String,
    pub status: String,
    pub metrics: Option<Value>,
    pub content_fingerprint: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LatestIndexLogRequest {
    pub provider_instance_id: Option<String>,
    pub project_root: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphGranularity {
    File,
    Class,
    Function,
    Method,
    Property,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphRequest {
    pub provider_id: Option<String>,
    pub project_root: Option<String>,
    pub target_path: Option<String>,
    pub granularity: GraphGranularity,
    pub recursive: Option<bool>,
    pub include_external: Option<bool>,
    pub include_first_neighbors: Option<bool>,
}
