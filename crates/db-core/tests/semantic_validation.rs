use lumvise_db_core::{
    LocalPersistence, SemanticArtifact, SemanticElement, SemanticOperation, SemanticPartition,
    SemanticPersistence, SemanticRelationship, SemanticStructureReconciliation,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: "src/a.rs".into(),
        element_kind: "file".into(),
        name: "A".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}
fn artifact() -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "note".into(),
        semantic_element_id: "a".into(),
        artifact_kind: "note".into(),
        title: "Note".into(),
        content_ref: None,
        content: Some("Text".into()),
        searchable_text: None,
        content_size_bytes: None,
        dependencies: vec![],
        metadata: json!({}),
    }
}
fn edge() -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/repo".into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }
}
fn sync(
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
) -> SemanticOperation {
    SemanticOperation::SyncStructure {
        project_root: "/repo".into(),
        elements,
        relationships,
    }
}
fn local_rejects(operation: SemanticOperation) -> bool {
    LocalPersistence::in_memory()
        .unwrap()
        .execute(operation, &InvocationControl::sixty_seconds())
        .is_err()
}
#[test]
fn shared_element_validation_matches_local_required_fields_and_fingerprint() {
    assert!(element("a").validate().is_ok());
    for field in [
        "project_root",
        "semantic_element_id",
        "semantic_source_id",
        "path",
        "element_kind",
        "name",
        "content_fingerprint",
    ] {
        let mut invalid = serde_json::to_value(element("a")).unwrap();
        invalid[field] = json!(" ");
        let invalid: SemanticElement = serde_json::from_value(invalid).unwrap();
        assert!(invalid.validate().is_err(), "{field}");
        assert!(local_rejects(sync(vec![invalid], vec![])), "{field}");
    }
}
#[test]
fn shared_relationship_validation_matches_local_required_fields() {
    assert!(edge().validate().is_ok());
    for field in [
        "project_root",
        "source_element_id",
        "target_element_id",
        "relationship_kind",
        "label",
    ] {
        let mut invalid = serde_json::to_value(edge()).unwrap();
        invalid[field] = json!("");
        let invalid: SemanticRelationship = serde_json::from_value(invalid).unwrap();
        assert!(invalid.validate().is_err(), "{field}");
        assert!(
            local_rejects(sync(vec![element("a"), element("b")], vec![invalid])),
            "{field}"
        );
    }
}
#[test]
fn shared_artifact_validation_matches_shell_and_owner_rules() {
    assert!(artifact().validate().is_ok());
    for field in [
        "artifact_id",
        "semantic_element_id",
        "artifact_kind",
        "title",
    ] {
        let mut invalid = serde_json::to_value(artifact()).unwrap();
        invalid[field] = json!("");
        let invalid: SemanticArtifact = serde_json::from_value(invalid).unwrap();
        assert!(invalid.validate().is_err());
        assert_eq!(
            invalid.validate_shell().is_ok(),
            field == "semantic_element_id"
        );
        assert!(local_rejects(SemanticOperation::UpsertArtifact {
            artifact: invalid,
            media_type: "text/plain".into()
        }));
    }
}
#[test]
fn shared_partition_validation_and_selection_match_existing_rules() {
    let partition = SemanticPartition {
        project_root: "/repo".into(),
        replace_paths: vec!["src/".into()],
    };
    assert!(partition.validate().is_ok());
    assert!(partition.contains_path("src/a.rs"));
    assert!(!partition.contains_path("src"));
    assert!(!partition.contains_path("src2/a.rs"));
    for partition in [
        SemanticPartition {
            project_root: " ".into(),
            replace_paths: vec!["src".into()],
        },
        SemanticPartition {
            project_root: "/repo".into(),
            replace_paths: vec![],
        },
        SemanticPartition {
            project_root: "/repo".into(),
            replace_paths: vec![" ".into()],
        },
    ] {
        assert!(partition.validate().is_err());
        assert!(local_rejects(SemanticOperation::SyncPartition {
            partition,
            elements: vec![],
            relationships: vec![]
        }));
    }
}
#[test]
fn shared_relationship_normalization_remaps_endpoints_and_keeps_parent_hint() {
    let mut existing = element("old");
    existing.content_fingerprint = Some("fp1:0000000000000001:hash".into());
    let mut incoming = existing.clone();
    incoming.semantic_element_id = "new".into();
    let mut child = element("child");
    child.parent_element_id = Some("new".into());
    let reconciled = SemanticStructureReconciliation::between(&[existing], &[incoming, child]);
    let mut explicit = edge();
    explicit.source_element_id = "new".into();
    explicit.target_element_id = "child".into();
    explicit.relationship_kind = "contains".into();
    explicit.label = "contains".into();
    explicit.metadata = json!({"explicit":true});
    let relationships = reconciled.normalized_relationships("/repo", &[explicit.clone(), explicit]);
    assert_eq!(relationships.len(), 1);
    assert_eq!(relationships[0].source_element_id, "old");
    assert_eq!(relationships[0].metadata, json!({"explicit":true}));
    assert_eq!(
        reconciled.active_elements[1].parent_element_id.as_deref(),
        Some("new")
    );
    let derived = reconciled.normalized_relationships("/repo", &[]);
    assert_eq!(
        derived[0].metadata,
        json!({"derived_from":"parent_element_id"})
    );
}
