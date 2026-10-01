//! Public count query stays project-scoped and crosses the existing binary transport.
use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

#[test]
fn counts_are_native_project_scoped_and_round_trip_without_element_records() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let root = "/project'with-quote";
    for project in [root, "/other"] {
        let elements = [("one","file"),("two","folder"),("three","file")].into_iter().map(|(id,kind)| {
            serde_json::from_value::<SemanticElement>(json!({"project_root":project,
                "semantic_element_id":format!("{project}/{id}"),"semantic_source_id":"test",
                "path":id,"element_kind":kind,"name":id,"lifecycle":"active","metadata":{"body":"x".repeat(10000)}})).unwrap()
        }).collect();
        persistence
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: project.into(),
                    elements,
                    relationships: vec![],
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
    }
    persistence
        .execute(
            SemanticOperation::RemoveElement {
                semantic_element_id: format!("{root}/two"),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let operation = SemanticOperation::ProjectElementCounts {
        project_root: root.into(),
    };
    let wire = CentralizedPersistence::encode_semantic_operation(operation).unwrap();
    let decoded = CentralizedPersistence::decode_semantic_operation(&wire).unwrap();
    let result = persistence
        .execute(decoded, &InvocationControl::sixty_seconds())
        .unwrap();
    let wire = CentralizedPersistence::encode_semantic_result(result).unwrap();
    let result = CentralizedPersistence::decode_semantic_result(&wire).unwrap();
    let SemanticResult::ProjectElementCounts {
        total_elements,
        elements_by_kind,
        commit_version,
        ..
    } = result
    else {
        panic!("expected grouped counts")
    };
    assert_eq!(total_elements, 3);
    assert_eq!(
        elements_by_kind.into_iter().collect::<Vec<_>>(),
        [("file".into(), 2), ("folder".into(), 1)]
    );
    assert!(commit_version > 0);
    let empty = persistence
        .execute(
            SemanticOperation::ProjectElementCounts {
                project_root: "/missing".into(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert!(matches!(
        empty,
        SemanticResult::ProjectElementCounts {
            total_elements: 0,
            ..
        }
    ));
}
