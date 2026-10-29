//! Knowledge-owned virtual documents, independent of source-file ownership.
//!
//! Plugins exchange these records through Knowledge's document exports. Semantic
//! indexing and record storage remain implementation details of that owner.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    KnowledgeArtifactLifecycleV2, KnowledgeArtifactStatusV2, KnowledgeWriteAction,
    KnowledgeWriteState,
};

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
    /// Explicit authoritative project and typed live/exact selection.
    Qualified(super::KnowledgeQualifiedCitation),
    /// A source or other indexed semantic element.
    SemanticElement {
        /// Stable semantic identity.
        semantic_element_id: String,
    },
    /// A Requirements or Specification artifact.
    Artifact {
        /// Stable artifact identity.
        artifact_id: String,
        /// Original observed working edit; frozen revisions can be cited without a counter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edit_version: Option<u64>,
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
    /// Internal resource owning this document's binary attachments, when present.
    /// This is not a user-facing Requirements or Specification artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_owner_id: Option<String>,
    /// Project or Agent Space owning this document.
    pub project_root: String,
    /// Knowledge-owned access restrictions; ordinary edits cannot replace these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<super::KnowledgeDocumentAccess>,
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
    /// Stable retry identity; reuse only with identical input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
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

/// Reads one known write without requiring its historical payload to remain available.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeDocumentWriteReceiptRequest {
    /// A route resolving to the operation's original durable project.
    pub project_root: String,
    /// The explicit retry identity supplied to the original write.
    pub operation_id: String,
    /// The original document write operation.
    pub action: KnowledgeWriteAction,
}

/// The exact locator accepted for a write; this reference does not pin its body.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeDocumentRevisionReference {
    /// Logical document identity.
    pub document_id: String,
    /// Original owning project route.
    pub project_root: String,
    /// Immutable accepted document locator, including a deletion marker when applicable.
    pub semantic_element_id: String,
}

/// A body-independent acknowledgement suitable for cooperating producer journals.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeDocumentWriteReceipt {
    /// The caller's original retry identity.
    pub operation_id: String,
    /// Knowledge's durable publication identity.
    pub publication_id: String,
    /// The route supplied for this scoped acknowledgement read.
    pub project_root: String,
    /// Whether the original write is unknown, pending, accepted or conflicted.
    pub state: KnowledgeWriteState,
    /// Accepted exact identity; available even after payload collection.
    pub document_revision: Option<KnowledgeDocumentRevisionReference>,
    /// Lifecycle counter captured with the accepted locator before discarding its body.
    pub lifecycle_revision: Option<u64>,
    /// Original semantic acceptance order, never the current graph's version.
    pub commit_version: Option<i64>,
}

/// Replaces only supplied fields after checking the caller's last-read revision.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct UpdateKnowledgeDocumentRequest {
    /// Stable retry identity; reuse only with identical input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Stable document identity.
    pub document_id: String,
    /// Project expected to own the document.
    pub project_root: String,
    /// Exact locator revision returned by the caller's last read.
    pub expected_semantic_element_id: String,
    /// Revision returned by the caller's last read.
    pub expected_revision: u64,
    /// Replacement title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Replacement content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Replacement grouping; an empty string selects the report root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Replacement searchable labels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Replacement citations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<KnowledgeDocumentReference>>,
    /// Replacement workflow metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Replacement lifecycle state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<KnowledgeArtifactStatusV2>,
    /// Document replacing this one when superseded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// Removes exactly the document revision the caller read.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct DeleteKnowledgeDocumentRequest {
    /// Stable retry identity; reuse only with identical input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub document_id: String,
    pub project_root: String,
    /// Exact locator revision returned by the caller's last read.
    pub expected_semantic_element_id: String,
    pub expected_revision: u64,
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

    #[test]
    fn document_update_serialization_omits_unrequested_edits() {
        let input = serde_json::json!({"document_id":"report","project_root":"/project",
            "expected_semantic_element_id":"revision:original","expected_revision":3,"content":""});
        let request: UpdateKnowledgeDocumentRequest =
            serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), input);
    }
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
                edit_version: Some(3),
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
