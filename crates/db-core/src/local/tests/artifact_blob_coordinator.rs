use lumvise_db_core::{DbCore, SemanticArtifact, SemanticElement};
use rusqlite::Connection;
use serde_json::json;

#[test]
fn failed_large_artifact_insert_removes_staged_sql_blob() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    let content = "new staged content ".repeat(700).into_bytes();

    let error = db
        .storage_manager()
        .semantic_storage()
        .upsert_artifact_content(&artifact("note", "missing-owner"), "text/plain", &content)
        .unwrap_err();
    drop(db);

    assert!(error.to_string().contains("existing semantic element id"));
    assert_eq!(blob_count_for_artifact(temp.path(), "note"), 0);
}

#[test]
fn failed_large_artifact_replacement_keeps_old_sql_blob() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("owner")).unwrap();
    let old_content = "old visible content ".repeat(700).into_bytes();
    let new_content = "new staged content ".repeat(700).into_bytes();
    storage
        .upsert_artifact_content(&artifact("note", "owner"), "text/plain", &old_content)
        .unwrap();
    let old_ref = storage
        .artifact("note")
        .unwrap()
        .unwrap()
        .content_ref
        .unwrap();

    let error = storage
        .upsert_artifact_content(
            &artifact("note", "missing-owner"),
            "text/plain",
            &new_content,
        )
        .unwrap_err();
    let blob = db.artifact_blobs().blob(&old_ref).unwrap().unwrap();
    drop(db);

    assert!(error.to_string().contains("existing semantic element id"));
    assert_eq!(blob.content, old_content);
    assert_eq!(blob_count_for_artifact(temp.path(), "note"), 1);
}

fn blob_count_for_artifact(path: &std::path::Path, artifact_id: &str) -> i64 {
    let conn = Connection::open(path).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM artifact_blobs WHERE artifact_id = ?1",
        [artifact_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: id.to_string(),
        element_kind: "file".to_string(),
        name: id.to_string(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact(id: &str, element_id: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.to_string(),
        semantic_element_id: element_id.to_string(),
        artifact_kind: "note".to_string(),
        title: id.to_string(),
        content_ref: None,
        content: None,
        searchable_text: None,
        content_size_bytes: None,
        metadata: json!({}),
        dependencies: vec![],
    }
}
