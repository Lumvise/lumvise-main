#![cfg(debug_assertions)]

use lumvise_db_core::{DbCore, SemanticElement};
use serde_json::json;

#[test]
fn persistent_graph_failed_write_does_not_publish_partial_mutation() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("old")).unwrap();

    let error = storage
        .upsert_element_with_failed_publish(&element("new"))
        .unwrap_err();

    assert!(error.to_string().contains("forced graph failure"));
    assert!(storage.element("old").unwrap().is_some());
    assert!(storage.element("new").unwrap().is_none());
    drop(db);

    let reopened = DbCore::open(temp.path()).unwrap();
    let reopened_storage = reopened.storage_manager().semantic_storage();
    assert!(reopened_storage.element("old").unwrap().is_some());
    assert!(reopened_storage.element("new").unwrap().is_none());
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: format!("src/{id}.rs"),
        element_kind: "file".to_string(),
        name: id.to_string(),
        parent_element_id: None,
        content_fingerprint: Some(format!("fp1:0000000000000001:{id}")),
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}
