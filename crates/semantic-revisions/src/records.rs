//! Semantic record shapes shared by persistence and the gateway policy engine.
use lumvise_contracts::ArtifactDependency;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticElement {
    pub project_root: String,
    pub semantic_element_id: String,
    pub semantic_source_id: String,
    pub path: String,
    pub element_kind: String,
    pub name: String,
    pub parent_element_id: Option<String>,
    /// Exact payload/evidence revisions needed to serve this element's body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revision_dependencies: Vec<crate::SemanticRecordRef>,
    pub content_fingerprint: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub lifecycle: String,
    #[serde(default)]
    pub match_evidence: Option<SemanticMatchEvidence>,
    #[serde(serialize_with = "crate::canonical_json::serialize_canonical_metadata")]
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
    #[serde(serialize_with = "crate::canonical_json::serialize_canonical_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticRelationship {
    pub project_root: String,
    pub source_element_id: String,
    pub target_element_id: String,
    pub relationship_kind: String,
    pub label: String,
    #[serde(serialize_with = "crate::canonical_json::serialize_canonical_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPartition {
    pub project_root: String,
    pub replace_paths: Vec<String>,
    /// Restrict publication to this producer; None retains authoritative path replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<String>,
}

impl SemanticElement {
    /// Reads the declared fingerprint scheme. Example: `element.fingerprint_algorithm()`.
    pub fn fingerprint_algorithm(&self) -> Option<&str> {
        self.metadata
            .get("fingerprint_algorithm")
            .and_then(Value::as_str)
    }
}
