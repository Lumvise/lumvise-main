use lumvise_db_core::{
    LocalPersistence, RelationalOperation, RelationalPersistence, RelationalResult,
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;

#[test]
fn plugin_host_open_persists_sql_and_semantic_graph() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("lumvise.db");
    let graph_path = directory.path().join("lumvise.db.semantic.grafeo");

    let persistence = LocalPersistence::open(&database_path).unwrap();
    let control = InvocationControl::sixty_seconds();
    RelationalPersistence::execute(
        &persistence,
        RelationalOperation::SetPersistentSetting {
            scope: "test".into(),
            key: "visible".into(),
            value: serde_json::json!(true),
        },
        &control,
    )
    .unwrap();
    SemanticPersistence::execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element()],
            relationships: Vec::new(),
        },
        &control,
    )
    .unwrap();
    drop(persistence);

    let reopened = LocalPersistence::open(&database_path).unwrap();
    let setting = RelationalPersistence::execute(
        &reopened,
        RelationalOperation::GetPersistentSetting {
            scope: "test".into(),
            key: "visible".into(),
        },
        &control,
    )
    .unwrap();
    assert!(matches!(
        setting,
        RelationalResult::PersistentSetting(Some(record))
            if record.value == serde_json::json!(true)
    ));
    let element = SemanticPersistence::execute(
        &reopened,
        SemanticOperation::Element {
            semantic_element_id: "element-1".into(),
        },
        &control,
    )
    .unwrap();
    assert!(matches!(
        element,
        SemanticResult::Element(Some(element)) if element.name == "lib.rs"
    ));
    assert!(graph_path.exists());
}

fn element() -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: "element-1".into(),
        semantic_source_id: "source-1".into(),
        path: "src/lib.rs".into(),
        element_kind: "file".into(),
        name: "lib.rs".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
    }
}
