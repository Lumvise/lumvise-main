use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, SemanticOperation, SemanticPersistence,
    SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn seed_structure(persistence: &LocalPersistence, root: &str) {
    let element = serde_json::from_value(json!({
        "project_root":root,"semantic_element_id":format!("{root}/owner"),
        "semantic_source_id":"search","path":"main.rs","name":"entry",
        "element_kind":"function","lifecycle":"active","metadata":{}
    }))
    .unwrap();
    persistence
        .execute(
            SemanticOperation::SyncStructure {
                project_root: root.into(),
                elements: vec![element],
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
}

#[test]
fn live_structure_routes_and_round_trips_on_the_existing_binary_transport() {
    let persistence = LocalPersistence::in_memory().unwrap();
    for root in ["/search", "/other"] {
        seed_structure(&persistence, root);
    }
    persistence.execute(
        SemanticOperation::UpsertArtifact {
            artifact: serde_json::from_value(json!({
                "artifact_id":"saved-note", "semantic_element_id":"/search/owner",
                "artifact_kind":"specification", "title":"Saved note",
                "content":"Saved artifact body must not enter structure-only search", "metadata":{}
            })).unwrap(),
            media_type: "text/plain".into(),
        },
        &InvocationControl::sixty_seconds(),
    ).unwrap();
    let SemanticResult::ProjectSnapshot(strong) = persistence
        .execute(
            SemanticOperation::ProjectSnapshot {
                scope: lumvise_db_core::ProjectSnapshotScope::ProjectRoot("/search".into()),
                artifact_namespace: None,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    else {
        panic!("expected published snapshot")
    };
    assert_eq!(strong.artifacts.len(), 1);
    let operation = SemanticOperation::SearchProjectStructure {
        project_root: "/search".into(),
    };
    assert_eq!(operation.project_root(), Some("/search"));
    let encoded = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
    let decoded = CentralizedPersistence::decode_semantic_operation(&encoded).unwrap();
    let result = persistence
        .execute(decoded, &InvocationControl::sixty_seconds())
        .unwrap();
    let encoded = CentralizedPersistence::encode_semantic_result(result).unwrap();
    let result = CentralizedPersistence::decode_semantic_result(&encoded).unwrap();
    let SemanticResult::ProjectSnapshot(snapshot) = result else {
        panic!("expected live project structure")
    };
    assert_eq!(snapshot.project_root, "/search");
    assert_eq!(snapshot.elements.len(), 1);
    assert_eq!(snapshot.elements[0].semantic_element_id, "/search/owner");
    assert!(snapshot.artifacts.is_empty());
    assert!(snapshot.commit_version > 0);
}

#[test]
fn live_search_watermark_is_restored_from_the_published_database_on_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("search.db");
    let operation = SemanticOperation::SearchProjectStructure {
        project_root: "/search".into(),
    };
    let persistence = LocalPersistence::open(&path).unwrap();
    seed_structure(&persistence, "/search");
    let before = persistence
        .execute(operation.clone(), &InvocationControl::sixty_seconds())
        .unwrap();
    drop(persistence);
    let persistence = LocalPersistence::open(&path).unwrap();
    let after = persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap();
    let (SemanticResult::ProjectSnapshot(before), SemanticResult::ProjectSnapshot(after)) =
        (before, after)
    else {
        panic!("expected live structure")
    };
    assert_eq!(before.commit_version, after.commit_version);
    assert_eq!(before.published_at, after.published_at);
}
