//! Version-2 contracts for durable Knowledge-owned artifacts.
//!
//! Knowledge artifact identifiers are caller-supplied strings. These types model
//! only records owned by `builtin.knowledge`; they are deliberately separate
//! from the legacy semantic-artifact CRUD contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable categories implemented by the Knowledge plugin.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeArtifactKindV2 {
    /// Product or technical specification.
    Specification,
    /// Tracked defect or concern.
    Issue,
    /// Assigned task.
    TaskAssignment,
    /// Domain definition.
    Definition,
    /// Contextual annotation.
    Annotation,
    /// Generated or authored report.
    Report,
    /// Durable decision.
    Decision,
    /// Manually authored note.
    ManualNote,
    /// Derived summary.
    DerivedSummary,
}

/// The target kind of a directed Knowledge artifact dependency.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "target_kind", rename_all = "snake_case")]
pub enum ArtifactDependencyTarget {
    /// A semantic element required by the dependent artifact.
    SemanticElement { semantic_element_id: String },
    /// Another Knowledge artifact required by the dependent artifact.
    Artifact { artifact_id: String },
}

/// A directed `depends_on` relation owned by one Knowledge artifact.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
pub struct ArtifactDependency {
    /// The semantic element or Knowledge artifact being depended on.
    pub target: ArtifactDependencyTarget,
}

impl ArtifactDependency {
    /// Validates target identity shape.
    pub fn validate(&self) -> Result<(), String> {
        let identifier = match &self.target {
            ArtifactDependencyTarget::SemanticElement {
                semantic_element_id,
            } => semantic_element_id,
            ArtifactDependencyTarget::Artifact { artifact_id } => artifact_id,
        };
        if identifier.trim().is_empty() {
            return Err("artifact dependency target identifier must be non-empty".into());
        }
        Ok(())
    }

    /// Returns the target identifier.
    pub fn target_id(&self) -> &str {
        match &self.target {
            ArtifactDependencyTarget::SemanticElement {
                semantic_element_id,
            } => semantic_element_id,
            ArtifactDependencyTarget::Artifact { artifact_id } => artifact_id,
        }
    }
}

/// Validates a complete dependency set for one artifact.
pub fn validate_artifact_dependencies(
    artifact_id: &str,
    dependencies: &[ArtifactDependency],
) -> Result<(), String> {
    if artifact_id.trim().is_empty() {
        return Err("artifact identifier must be non-empty".into());
    }
    let mut seen = std::collections::HashSet::new();
    for dependency in dependencies {
        dependency.validate()?;
        if matches!(
            &dependency.target,
            ArtifactDependencyTarget::Artifact { artifact_id: target_id } if target_id == artifact_id
        ) {
            return Err("artifact dependency cannot target its own artifact".into());
        }
        if !seen.insert(dependency) {
            return Err("artifact dependencies must not contain duplicates".into());
        }
    }
    Ok(())
}

/// Persistence-neutral durable Knowledge record.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct KnowledgeArtifactV2 {
    /// Stable string artifact identity.
    pub artifact_id: String,
    /// Narrow semantic owner identity.
    pub semantic_element_id: String,
    /// Typed Knowledge classification.
    pub knowledge_type: KnowledgeArtifactKindV2,
    /// Human-readable title.
    pub title: String,
    /// Durable content.
    pub content: String,
    /// Searchable labels.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Directed semantic/artifact dependencies.
    #[serde(default)]
    pub dependencies: Vec<ArtifactDependency>,
    /// Plugin-neutral extension metadata.
    #[serde(default)]
    pub metadata: Value,
    /// Optional knowledge-space path that groups this artifact, relative to
    /// the project knowledge root (for example `decisions/auth/jwt.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional project filter value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}

/// Request for creating one durable Knowledge artifact.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CreateKnowledgeArtifactRequestV2 {
    /// Caller-supplied stable string artifact identity.
    pub artifact_id: String,
    /// Semantic element that owns the artifact.
    pub semantic_element_id: String,
    /// Typed Knowledge classification.
    pub knowledge_type: KnowledgeArtifactKindV2,
    /// Human-readable title.
    pub title: String,
    /// Durable content.
    pub content: String,
    /// Searchable labels.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Directed semantic/artifact dependencies.
    #[serde(default)]
    pub dependencies: Vec<ArtifactDependency>,
    /// Plugin-neutral extension metadata.
    #[serde(default)]
    pub metadata: Value,
    /// Optional knowledge-space path that groups this artifact, relative to
    /// the project knowledge root (for example `decisions/auth/jwt.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional project filter value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}

/// Response after creating one durable Knowledge artifact.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CreateKnowledgeArtifactResponseV2 {
    /// The persisted artifact.
    pub artifact: KnowledgeArtifactV2,
}

/// Request for updating one durable Knowledge artifact.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct UpdateKnowledgeArtifactRequestV2 {
    /// Stable string identity of the artifact to update.
    pub artifact_id: String,
    /// Replacement semantic owner identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_element_id: Option<String>,
    /// Replacement Knowledge classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_type: Option<KnowledgeArtifactKindV2>,
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Replacement content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Replacement searchable labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Replacement directed dependencies. Absent keeps the existing set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<ArtifactDependency>>,
    /// Replacement plugin-neutral extension metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Replacement knowledge-space path. Absent keeps the existing path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Replacement project filter value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}

/// Response after updating one durable Knowledge artifact.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct UpdateKnowledgeArtifactResponseV2 {
    /// The persisted artifact.
    pub artifact: KnowledgeArtifactV2,
}

/// Request for deleting one durable Knowledge artifact within a project.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DeleteKnowledgeArtifactRequestV2 {
    /// Stable string identity of the artifact to delete.
    pub artifact_id: String,
    /// Project scope that must own the artifact.
    pub project_root: String,
}

/// Response after attempting a durable Knowledge artifact deletion.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DeleteKnowledgeArtifactResponseV2 {
    /// Stable string identity of the requested artifact.
    pub artifact_id: String,
    /// Whether an artifact was removed.
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_crud_contracts_round_trip_string_artifact_ids() {
        let request = CreateKnowledgeArtifactRequestV2 {
            artifact_id: "artifact:architecture/42".into(),
            semantic_element_id: "file:src/lib.rs".into(),
            knowledge_type: KnowledgeArtifactKindV2::Decision,
            title: "Choose string identifiers".into(),
            content: "Artifact identifiers remain opaque strings.".into(),
            tags: vec!["architecture".into()],
            dependencies: vec![],
            metadata: serde_json::json!({"source": "test"}),
            path: Some("decisions/identifiers.md".into()),
            project_root: Some("/project".into()),
        };
        let encoded = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(encoded["artifact_id"], "artifact:architecture/42");
        assert_eq!(encoded["path"], "decisions/identifiers.md");
        assert_eq!(
            serde_json::from_value::<CreateKnowledgeArtifactRequestV2>(encoded)
                .expect("deserialize request"),
            request
        );
    }

    #[test]
    fn update_request_omits_unspecified_fields_and_serializes_full_updates() {
        let partial = UpdateKnowledgeArtifactRequestV2 {
            artifact_id: "artifact:annotation:42".into(),
            semantic_element_id: None,
            knowledge_type: None,
            title: Some("Updated artifact".into()),
            content: Some("Updated content".into()),
            tags: Some(vec!["live".into()]),
            dependencies: None,
            metadata: None,
            path: None,
            project_root: None,
        };
        assert_eq!(
            serde_json::to_value(partial).expect("serialize partial update"),
            serde_json::json!({
                "artifact_id": "artifact:annotation:42",
                "content": "Updated content",
                "tags": ["live"],
                "title": "Updated artifact"
            })
        );

        let full = UpdateKnowledgeArtifactRequestV2 {
            artifact_id: "artifact:annotation:42".into(),
            semantic_element_id: Some("file:src/lib.rs".into()),
            knowledge_type: Some(KnowledgeArtifactKindV2::Annotation),
            title: Some("Updated artifact".into()),
            content: Some("Updated content".into()),
            tags: Some(vec!["live".into()]),
            dependencies: Some(vec![]),
            metadata: Some(serde_json::json!({"source": "test"})),
            path: Some("notes/updated.md".into()),
            project_root: Some("/project".into()),
        };
        assert_eq!(
            serde_json::to_value(full).expect("serialize full update"),
            serde_json::json!({
                "artifact_id": "artifact:annotation:42",
                "semantic_element_id": "file:src/lib.rs",
                "knowledge_type": "annotation",
                "title": "Updated artifact",
                "dependencies": [],
                "content": "Updated content",
                "tags": ["live"],
                "metadata": {"source": "test"},
                "path": "notes/updated.md",
                "project_root": "/project"
            })
        );
    }

    #[test]
    fn delete_response_preserves_requested_string_identifier() {
        let response = DeleteKnowledgeArtifactResponseV2 {
            artifact_id: "artifact:missing".into(),
            deleted: false,
        };
        assert_eq!(
            serde_json::to_value(response).expect("serialize response"),
            serde_json::json!({"artifact_id": "artifact:missing", "deleted": false})
        );
    }
}
