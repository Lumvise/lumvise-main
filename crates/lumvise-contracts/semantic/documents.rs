//! Knowledge-owned virtual documents, independent of source-file ownership.
//!
//! Plugins exchange these records through Knowledge's document exports. Semantic
//! indexing and record storage remain implementation details of that owner.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{KnowledgeArtifactLifecycleV2, KnowledgeArtifactStatusV2};

/// Presentation and owning workflow of a virtual document.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDocumentKind {
    /// Independently browsable Markdown report.
    Report,
    /// A Canvas-owned drawing whose scene is the document content.
    Canvas,
    /// A background agent's instructions.
    Ruleset,
    /// Readable projection of an agent's ordered missions and their outcomes.
    MissionLog,
    /// Reference material, including retained source-derived context.
    Context,
}

/// An optional citation; its target does not own the citing document.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "target_kind", rename_all = "snake_case")]
pub enum KnowledgeDocumentReference {
    /// A source or other indexed semantic element.
    SemanticElement {
        /// Stable semantic identity.
        semantic_element_id: String,
    },
    /// A Requirements or Specification artifact.
    Artifact {
        /// Stable artifact identity.
        artifact_id: String,
    },
    /// Another independent document.
    Document {
        /// Stable document identity.
        document_id: String,
    },
}

/// One independently addressable document with optional source references.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct KnowledgeDocument {
    /// Stable, caller-supplied document identity.
    pub document_id: String,
    /// Index identity for this document, not an owning source-file anchor.
    pub semantic_element_id: String,
    /// Project or Agent Space owning this document.
    pub project_root: String,
    /// Document workflow and presentation.
    pub document_kind: KnowledgeDocumentKind,
    /// Human-readable title.
    pub title: String,
    /// Markdown text or the owning workflow's serialized content.
    pub content: String,
    /// Optional organization path, independent of source containment.
    #[serde(default)]
    pub path: Option<String>,
    /// Search and workflow labels.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional citations, which may outlive their targets.
    #[serde(default)]
    pub references: Vec<KnowledgeDocumentReference>,
    /// Workflow-owned metadata, preserved by partial updates.
    #[serde(default)]
    pub metadata: Value,
    /// Shared Knowledge lifecycle and revision fields.
    #[serde(flatten)]
    pub lifecycle: KnowledgeArtifactLifecycleV2,
}

/// Creates a document without selecting a source element or filesystem path.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct CreateKnowledgeDocumentRequest {
    /// Stable document identity.
    pub document_id: String,
    /// Exact owning project root or Agent Space identity.
    pub project_root: String,
    /// Document workflow.
    pub document_kind: KnowledgeDocumentKind,
    /// Human-readable title.
    pub title: String,
    /// Initial document content; empty drafts are valid.
    pub content: String,
    /// Optional report/board grouping path.
    #[serde(default)]
    pub path: Option<String>,
    /// Initial searchable labels.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional citations.
    #[serde(default)]
    pub references: Vec<KnowledgeDocumentReference>,
    /// Workflow-owned metadata.
    #[serde(default)]
    pub metadata: Value,
}

/// Identifies a document inside its owning project for reads or deletion.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct KnowledgeDocumentIdentity {
    /// Stable document identity.
    pub document_id: String,
    /// Project expected to own the document.
    pub project_root: String,
}

/// Replaces only supplied fields after checking the caller's last-read revision.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct UpdateKnowledgeDocumentRequest {
    /// Stable document identity.
    pub document_id: String,
    /// Project expected to own the document.
    pub project_root: String,
    /// Revision returned by the caller's last read.
    pub expected_revision: u64,
    /// Replacement title.
    pub title: Option<String>,
    /// Replacement content.
    pub content: Option<String>,
    /// Replacement grouping; an empty string selects the report root.
    pub path: Option<String>,
    /// Replacement searchable labels.
    pub tags: Option<Vec<String>>,
    /// Replacement citations.
    pub references: Option<Vec<KnowledgeDocumentReference>>,
    /// Replacement workflow metadata.
    pub metadata: Option<Value>,
    /// Replacement lifecycle state.
    pub status: Option<KnowledgeArtifactStatusV2>,
    /// Document replacing this one when superseded.
    pub superseded_by: Option<String>,
}

/// Lists or searches independent documents, without requiring an anchor.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ListKnowledgeDocumentsRequest {
    /// Exact project scope; omission selects accessible non-agent projects.
    pub project_root: Option<String>,
    /// Optional document workflow filter.
    pub document_kind: Option<KnowledgeDocumentKind>,
    /// Optional text query for document title/content/path/tags.
    pub query: Option<String>,
    /// Include archived, resolved or superseded documents.
    #[serde(default)]
    pub include_inactive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_report_request_round_trips_without_a_source_anchor() {
        let request: CreateKnowledgeDocumentRequest = serde_json::from_value(json!({
            "document_id": "report:overview", "project_root": "/project",
            "document_kind": "report", "title": "Overview", "content": "# Overview"
        }))
        .expect("independent report request");
        let encoded = serde_json::to_value(&request).expect("encode request");
        assert_eq!(request.references, vec![]);
        assert!(encoded.get("semantic_element_id").is_none());
        assert_eq!(
            serde_json::from_value::<CreateKnowledgeDocumentRequest>(encoded)
                .expect("round-trip request"),
            request
        );
    }

    #[test]
    fn references_preserve_their_distinct_identity_categories() {
        let references = vec![
            KnowledgeDocumentReference::SemanticElement {
                semantic_element_id: "file:src/lib.rs".into(),
            },
            KnowledgeDocumentReference::Artifact {
                artifact_id: "requirements:search".into(),
            },
            KnowledgeDocumentReference::Document {
                document_id: "report:search".into(),
            },
        ];
        let encoded = serde_json::to_value(&references).expect("encode references");
        assert_eq!(encoded[2]["target_kind"], "document");
        assert_eq!(
            serde_json::from_value::<Vec<KnowledgeDocumentReference>>(encoded)
                .expect("round-trip references"),
            references
        );
    }
}
