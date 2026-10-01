//! Version-2 semantic HTTP contracts matching the repo2 data model.
//!
//! These are the canonical request shapes for the daemon's semantic read
//! routes, served by the `builtin.semantic` plugin as signed HTTP exports:
//!
//! - `POST /api/context` — [`SemanticContextRequestV2`]
//! - `POST /api/search/context` — [`SearchContextRequestV2`]
//! - `POST /api/semantic-relationship-tree` — [`RelationshipTreeRequestV2`]
//!
//! (string-keyed elements, no legacy numeric artifact ids). The response
//! contracts below are the shared wire types for those same routes. They use
//! only repo2 semantic fields.

use schemars::JsonSchema;
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};
use serde_json::Value;

/// Request for `POST /api/context` (paged full-fidelity project context).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SemanticContextRequestV2 {
    pub project_root: String,
    #[serde(default)]
    pub root_element_id: Option<String>,
    #[serde(default)]
    pub include_descendants: Option<bool>,
    /// One of `elements`, `relationships`, `artifacts`.
    pub record_kind: String,
}

/// Request for `POST /api/search/context` (semantic element search).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SearchContextRequestV2 {
    #[serde(default)]
    pub project_root: Option<String>,
    pub query: String,
    #[serde(default)]
    pub element_kind: Option<String>,
    #[serde(default)]
    pub include_inactive: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Request for `POST /api/semantic-relationship-tree`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RelationshipTreeRequestV2 {
    pub semantic_element_id: String,
    /// `dependencies`, `dependents`, or `both` (handler default).
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub max_depth: Option<usize>,
    #[serde(default)]
    pub include_descendants: Option<bool>,
    #[serde(default)]
    pub include_inactive: Option<bool>,
}

/// Public semantic-element projection.
///
/// Tree and search responses use the required locator fields. Context pages
/// additionally populate the optional full-fidelity fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SemanticElementV2 {
    pub semantic_element_id: String,
    pub element_kind: String,
    pub name: String,
    pub path: String,
    pub parent_element_id: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Public semantic-relationship projection.
///
/// Tree responses use its relationship-view fields; context pages also carry
/// the optional persistence fields.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SemanticRelationshipV2 {
    pub source_element_id: String,
    pub target_element_id: String,
    pub relationship_kind: String,
    pub label: String,
    #[serde(default)]
    pub target_label: Option<String>,
    #[serde(default)]
    pub target_locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Indexed semantic artifact returned in a context page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SemanticIndexedArtifactV2 {
    pub project_root: String,
    /// Stable artifact identity. Artifact IDs are strings in repo2.
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub artifact_kind: String,
    pub title: String,
    pub content_ref: Option<String>,
    pub content: Option<String>,
    pub searchable_text: Option<String>,
    pub content_size_bytes: Option<usize>,
    pub metadata: Value,
}

/// Typed complete full-fidelity semantic records.
#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq)]
pub struct SemanticContextResponseV2 {
    pub record_kind: String,
    pub elements: Vec<SemanticElementV2>,
    pub relationships: Vec<SemanticRelationshipV2>,
    pub artifacts: Vec<SemanticIndexedArtifactV2>,
    pub commit_version: i64,
    pub published_at: String,
}

impl Serialize for SemanticContextResponseV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SemanticContextResponseV2", 6)?;
        state.serialize_field("record_kind", &self.record_kind)?;
        state.serialize_field("elements", &FullSemanticElements(&self.elements))?;
        state.serialize_field(
            "relationships",
            &FullSemanticRelationships(&self.relationships),
        )?;
        state.serialize_field("artifacts", &self.artifacts)?;
        state.serialize_field("commit_version", &self.commit_version)?;
        state.serialize_field("published_at", &self.published_at)?;
        state.end()
    }
}

struct FullSemanticElements<'a>(&'a [SemanticElementV2]);

impl Serialize for FullSemanticElements<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for element in self.0 {
            sequence.serialize_element(&FullSemanticElement::from(element))?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct FullSemanticElement<'a> {
    semantic_element_id: &'a String,
    element_kind: &'a String,
    name: &'a String,
    path: &'a String,
    parent_element_id: &'a Option<String>,
    start_line: &'a Option<i64>,
    end_line: &'a Option<i64>,
    project_root: &'a Option<String>,
    semantic_source_id: &'a Option<String>,
    content_fingerprint: &'a Option<String>,
    lifecycle: &'a Option<String>,
    metadata: &'a Option<Value>,
}

impl<'a> From<&'a SemanticElementV2> for FullSemanticElement<'a> {
    fn from(element: &'a SemanticElementV2) -> Self {
        Self {
            semantic_element_id: &element.semantic_element_id,
            element_kind: &element.element_kind,
            name: &element.name,
            path: &element.path,
            parent_element_id: &element.parent_element_id,
            start_line: &element.start_line,
            end_line: &element.end_line,
            project_root: &element.project_root,
            semantic_source_id: &element.semantic_source_id,
            content_fingerprint: &element.content_fingerprint,
            lifecycle: &element.lifecycle,
            metadata: &element.metadata,
        }
    }
}

struct FullSemanticRelationships<'a>(&'a [SemanticRelationshipV2]);

impl Serialize for FullSemanticRelationships<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for relationship in self.0 {
            sequence.serialize_element(&FullSemanticRelationship::from(relationship))?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct FullSemanticRelationship<'a> {
    source_element_id: &'a String,
    target_element_id: &'a String,
    relationship_kind: &'a String,
    label: &'a String,
    project_root: &'a Option<String>,
    lifecycle: &'a Option<String>,
    metadata: &'a Option<Value>,
}

impl<'a> From<&'a SemanticRelationshipV2> for FullSemanticRelationship<'a> {
    fn from(relationship: &'a SemanticRelationshipV2) -> Self {
        Self {
            source_element_id: &relationship.source_element_id,
            target_element_id: &relationship.target_element_id,
            relationship_kind: &relationship.relationship_kind,
            label: &relationship.label,
            project_root: &relationship.project_root,
            lifecycle: &relationship.lifecycle,
            metadata: &relationship.metadata,
        }
    }
}

/// One semantic search hit.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SemanticSearchResultV2 {
    pub element: SemanticElementV2,
    pub score: f32,
}

/// Typed response for semantic element search.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SearchContextResponseV2 {
    pub query: String,
    pub project_root: Option<String>,
    pub mode: String,
    pub index_state: String,
    pub candidate_count: usize,
    pub results: Vec<SemanticSearchResultV2>,
}

/// One recursively reachable element in a relationship tree.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RelationshipTreeNodeV2 {
    pub element: SemanticElementV2,
    pub dependencies: Vec<RelationshipTreeBranchV2>,
    pub dependents: Vec<RelationshipTreeBranchV2>,
}

/// A relationship edge and its recursively reachable target.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RelationshipTreeBranchV2 {
    pub relationship: SemanticRelationshipV2,
    pub node: RelationshipTreeNodeV2,
    pub cycle: bool,
}

/// Typed response for `POST /api/semantic-relationship-tree`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RelationshipTreeResponseV2 {
    pub project_root: String,
    pub semantic_element_id: String,
    /// `dependencies`, `dependents`, or `both`.
    pub direction: String,
    pub total_nodes: usize,
    pub max_depth: usize,
    pub root: RelationshipTreeNodeV2,
    pub commit_version: i64,
    pub published_at: String,
}

#[cfg(test)]
mod tests {
    use schemars::schema_for;
    use serde_json::json;

    use super::{RelationshipTreeResponseV2, SearchContextResponseV2, SemanticContextResponseV2};

    #[test]
    fn v2_response_fixtures_round_trip_and_have_schemas() {
        let context = json!({
            "record_kind": "artifacts",
            "elements": [],
            "relationships": [],
            "artifacts": [{
                "project_root": "/work/demo",
                "artifact_id": "source:parse",
                "semantic_element_id": "fn:parse",
                "artifact_kind": "source",
                "title": "Parse source",
                "content_ref": null,
                "content": "fn parse() {}",
                "searchable_text": "parse",
                "content_size_bytes": 13,
                "metadata": {"language": "rust"}
            }],
            "commit_version": 7,
            "published_at": "2026-07-22T12:00:00Z"
        });
        let search = json!({
            "query": "parse",
            "project_root": "/work/demo",
            "mode": "lexical_fallback",
            "index_state": "missing",
            "candidate_count": 1,
            "results": [{
                "element": {
                    "semantic_element_id": "fn:parse",
                    "element_kind": "function",
                    "name": "parse",
                    "path": "src/parser.rs",
                    "parent_element_id": "file:parser",
                    "start_line": 1,
                    "end_line": 2
                },
                "score": 1.0
            }]
        });
        let tree = json!({
            "project_root": "/work/demo",
            "semantic_element_id": "fn:parse",
            "direction": "dependencies",
            "total_nodes": 2,
            "max_depth": 3,
            "commit_version": 7,
            "published_at": "2026-07-22T12:00:00Z",
            "root": {
                "element": {
                    "semantic_element_id": "fn:parse",
                    "element_kind": "function",
                    "name": "parse",
                    "path": "src/parser.rs",
                    "parent_element_id": "file:parser",
                    "start_line": 1,
                    "end_line": 2
                },
                "dependencies": [{
                    "relationship": {
                        "source_element_id": "fn:parse",
                        "target_element_id": "fn:render",
                        "relationship_kind": "calls",
                        "label": "calls",
                        "target_label": "render",
                        "target_locator": "crate::render"
                    },
                    "node": {
                        "element": {
                            "semantic_element_id": "fn:render",
                            "element_kind": "function",
                            "name": "render",
                            "path": "src/render.rs",
                            "parent_element_id": null,
                            "start_line": null,
                            "end_line": null
                        },
                        "dependencies": [],
                        "dependents": []
                    },
                    "cycle": false
                }],
                "dependents": []
            }
        });

        for (value, decoded) in [
            (
                context.clone(),
                serde_json::to_value(
                    serde_json::from_value::<SemanticContextResponseV2>(context)
                        .expect("context fixture"),
                )
                .expect("serialize context"),
            ),
            (
                search.clone(),
                serde_json::to_value(
                    serde_json::from_value::<SearchContextResponseV2>(search)
                        .expect("search fixture"),
                )
                .expect("serialize search"),
            ),
            (
                tree.clone(),
                serde_json::to_value(
                    serde_json::from_value::<RelationshipTreeResponseV2>(tree)
                        .expect("tree fixture"),
                )
                .expect("serialize tree"),
            ),
        ] {
            assert_eq!(decoded, value);
        }

        assert!(
            schema_for!(SemanticContextResponseV2)
                .schema
                .object
                .is_some()
        );
        assert!(schema_for!(SearchContextResponseV2).schema.object.is_some());
        assert!(
            schema_for!(RelationshipTreeResponseV2)
                .schema
                .object
                .is_some()
        );
    }
}
