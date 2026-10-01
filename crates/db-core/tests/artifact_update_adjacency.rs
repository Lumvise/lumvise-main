//! Artifact replacement keeps incoming and outgoing links on their existing owners.
use lumvise_db_core::{
    LocalPersistence, SemanticArtifact, SemanticElement, SemanticOperation, SemanticPersistence,
    SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn artifact(id: &str, target: Option<&str>) -> SemanticArtifact {
    serde_json::from_value(json!({"artifact_id":id,"semantic_element_id":"owner",
        "artifact_kind":"knowledge","title":id,"content":id,"metadata":{},
        "dependencies":target.into_iter().map(|id| json!({"target":{"target_kind":"artifact","artifact_id":id}})).collect::<Vec<_>>()
    })).unwrap()
}

#[test]
fn replacement_preserves_both_dependency_directions_and_owner_attachments() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let owner: SemanticElement = serde_json::from_value(json!({"project_root":"/adjacency",
        "semantic_element_id":"owner","semantic_source_id":"test","path":"owner.rs",
        "element_kind":"file","name":"owner","lifecycle":"active","metadata":{}}))
    .unwrap();
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/adjacency".into(),
            elements: vec![owner],
            relationships: vec![],
        },
    );
    for record in [
        artifact("target", None),
        artifact("middle", Some("target")),
        artifact("inbound", Some("middle")),
        artifact("unrelated", Some("target")),
    ] {
        execute(
            &persistence,
            SemanticOperation::UpsertArtifact {
                artifact: record,
                media_type: "text/plain".into(),
            },
        );
    }
    let mut replacement = artifact("middle", Some("target"));
    replacement.content = Some("updated body".into());
    execute(
        &persistence,
        SemanticOperation::UpsertArtifact {
            artifact: replacement,
            media_type: "text/plain".into(),
        },
    );
    for (target, expected) in [
        ("middle", vec!["inbound"]),
        ("target", vec!["middle", "unrelated"]),
    ] {
        let SemanticResult::Artifacts(records) = execute(
            &persistence,
            SemanticOperation::ArtifactDependents {
                target_kind: "artifact".into(),
                target_id: target.into(),
            },
        ) else {
            panic!("expected artifacts")
        };
        assert_eq!(
            records
                .iter()
                .map(|record| record.artifact_id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
    let SemanticResult::Artifacts(records) = execute(
        &persistence,
        SemanticOperation::ArtifactsForElements {
            semantic_element_ids: ["owner".into()].into_iter().collect(),
        },
    ) else {
        panic!("expected artifacts")
    };
    assert_eq!(records.len(), 4);
    let SemanticResult::Artifacts(project_records) = execute(
        &persistence,
        SemanticOperation::ProjectArtifacts {
            project_root: "/adjacency".into(),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected project artifacts")
    };
    assert_eq!(project_records, records);
    let SemanticResult::Artifacts(foreign_records) = execute(
        &persistence,
        SemanticOperation::ProjectArtifacts {
            project_root: "/unrelated".into(),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected project artifacts")
    };
    assert!(foreign_records.is_empty());
    assert_eq!(
        records
            .iter()
            .find(|record| record.artifact_id == "middle")
            .unwrap()
            .content
            .as_deref(),
        Some("updated body")
    );
}
