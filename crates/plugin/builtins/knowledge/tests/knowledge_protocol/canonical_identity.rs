use super::*;

#[test]
fn create_returns_the_stored_identity_and_preserves_an_unrelated_local_record() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.put_knowledge("note", "file:local", "Local note");
    broker.ensure_element("file:remote", "/other");
    *broker.canonical_artifact_id.lock().unwrap() = Some("qualified:note".into());
    let system = start_packaged_knowledge(&workspace, broker.clone());
    let created = invoke_value(&system, CREATE_EXPORT_ID, serde_json::json!({
        "artifact_id": "note", "semantic_element_id": "file:remote", "project_root": "/other",
        "knowledge_type": "definition", "title": "Online note", "content": "Online information"
    }));
    assert_eq!(created["artifact"]["artifact_id"], "qualified:note");
    assert_eq!(created["artifact"]["revision"], 1);
    let read = invoke_value(&system, GET_EXPORT_ID, serde_json::json!({"artifact_id": "qualified:note"}));
    assert_eq!(read["artifact"]["title"], "Online note");
    let local = invoke_value(&system, GET_EXPORT_ID, serde_json::json!({"artifact_id": "note"}));
    assert_eq!(local["artifact"]["title"], "Local note");
}
