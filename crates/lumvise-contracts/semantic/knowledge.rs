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
    /// Resumable work state or handoff between agents or sessions.
    Checkpoint,
    /// Open question awaiting an answer.
    Question,
}

/// Lifecycle state of a Knowledge artifact.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeArtifactStatusV2 {
    /// Current and applicable.
    #[default]
    Active,
    /// Completed or answered; kept as history.
    Resolved,
    /// Replaced by the artifact named in `superseded_by`.
    Superseded,
    /// Withdrawn or no longer relevant.
    Archived,
}

/// Lifecycle and version state maintained on every Knowledge record. Its
/// fields sit beside the record's other fields on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct KnowledgeArtifactLifecycleV2 {
    /// Lifecycle state. Records stored before lifecycle support read as active.
    #[serde(default)]
    pub status: KnowledgeArtifactStatusV2,
    /// RFC 3339 instant of the last status change; absent while the status
    /// never changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_changed_at: Option<String>,
    /// RFC 3339 instant after which an active artifact stops counting as
    /// active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Artifact that replaces this one; present exactly when the status is
    /// superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Write counter: 1 after the first stored write, +1 for each later one.
    /// 0 marks a record stored before revisions existed.
    #[serde(default)]
    pub revision: u64,
    /// RFC 3339 creation instant; absent on records stored before timestamps
    /// existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC 3339 instant of the last write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl KnowledgeArtifactLifecycleV2 {
    /// Validates the status-dependent fields of the record `artifact_id`.
    pub fn validate(&self, artifact_id: &str) -> Result<(), String> {
        let superseded = self.status == KnowledgeArtifactStatusV2::Superseded;
        match self.superseded_by.as_deref() {
            None if superseded => {
                return Err("status `superseded` requires `superseded_by`".into());
            }
            Some(_) if !superseded => {
                return Err("`superseded_by` requires status `superseded`".into());
            }
            Some(target) if target.trim().is_empty() => {
                return Err("`superseded_by` must be a non-empty artifact identifier".into());
            }
            Some(target) if target == artifact_id => {
                return Err("an artifact cannot supersede itself".into());
            }
            _ => {}
        }
        if self.expires_at.is_some() && self.status != KnowledgeArtifactStatusV2::Active {
            return Err("`expires_at` applies only to status `active`".into());
        }
        Ok(())
    }
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
    /// Lifecycle and version state.
    #[serde(flatten)]
    pub lifecycle: KnowledgeArtifactLifecycleV2,
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
    /// Optional RFC 3339 instant after which the artifact stops counting as
    /// active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
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
    /// Replacement lifecycle status. A supplied status drops the stored
    /// `expires_at`, and one other than superseded drops `superseded_by`,
    /// unless this request supplies new values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<KnowledgeArtifactStatusV2>,
    /// Artifact that replaces this one; requires status superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Replacement RFC 3339 expiry; requires status active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Revision the caller last read. The update is rejected when the stored
    /// revision differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
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
            expires_at: None,
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
            status: None,
            superseded_by: None,
            expires_at: None,
            expected_revision: None,
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
            status: Some(KnowledgeArtifactStatusV2::Superseded),
            superseded_by: Some("artifact:annotation:43".into()),
            expires_at: None,
            expected_revision: Some(3),
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
                "project_root": "/project",
                "status": "superseded",
                "superseded_by": "artifact:annotation:43",
                "expected_revision": 3
            })
        );
    }

    #[test]
    fn record_without_lifecycle_fields_reads_as_active_revision_zero() {
        let stored = serde_json::json!({
            "artifact_id": "artifact:task:1", "semantic_element_id": "file:src/lib.rs",
            "knowledge_type": "task_assignment", "title": "Fix parser", "content": "Body"
        });
        let mut artifact: KnowledgeArtifactV2 =
            serde_json::from_value(stored).expect("deserialize record without lifecycle");
        assert_eq!(artifact.lifecycle, KnowledgeArtifactLifecycleV2::default());

        artifact.lifecycle = KnowledgeArtifactLifecycleV2 {
            status: KnowledgeArtifactStatusV2::Resolved,
            status_changed_at: Some("2026-10-13T08:00:00+00:00".into()),
            revision: 2,
            ..Default::default()
        };
        let encoded = serde_json::to_value(&artifact).expect("serialize record");
        assert_eq!(encoded["status"], "resolved");
        assert_eq!(encoded["status_changed_at"], "2026-10-13T08:00:00+00:00");
        assert_eq!(encoded["revision"], 2);
        assert!(encoded.get("lifecycle").is_none());
        assert_eq!(
            serde_json::from_value::<KnowledgeArtifactV2>(encoded).expect("round trip"),
            artifact
        );
    }

    #[test]
    fn lifecycle_validation_ties_superseded_by_and_expiry_to_status() {
        let lifecycle = |status, superseded_by: Option<&str>, expires_at: Option<&str>| {
            KnowledgeArtifactLifecycleV2 {
                status,
                superseded_by: superseded_by.map(str::to_owned),
                expires_at: expires_at.map(str::to_owned),
                ..Default::default()
            }
            .validate("artifact:a")
        };
        use KnowledgeArtifactStatusV2::{Active, Resolved, Superseded};
        assert!(lifecycle(Active, None, Some("2027-01-01T00:00:00Z")).is_ok());
        assert!(lifecycle(Superseded, Some("artifact:b"), None).is_ok());
        assert!(lifecycle(Superseded, None, None).is_err());
        assert!(lifecycle(Superseded, Some("artifact:a"), None).is_err());
        assert!(lifecycle(Superseded, Some(" "), None).is_err());
        assert!(lifecycle(Resolved, Some("artifact:b"), None).is_err());
        assert!(lifecycle(Resolved, None, Some("2027-01-01T00:00:00Z")).is_err());
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
