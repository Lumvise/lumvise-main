use lumvise_db_core::{DbCore, SemanticArtifact, SemanticElement, SemanticRelationship};
use serde_json::json;

#[test]
fn semantic_elements_can_be_inserted_and_removed_with_owned_graph_rows() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("root", None)).unwrap();
    storage
        .upsert_element(&element("child", Some("root")))
        .unwrap();
    storage.upsert_artifact(&artifact("note", "child")).unwrap();
    storage
        .link_elements(&relationship("root", "child"))
        .unwrap();

    let removed = storage.remove_element("child").unwrap();

    assert!(removed);
    assert!(storage.element("child").unwrap().is_none());
    assert!(storage.artifact("note").unwrap().is_none());
    assert!(storage.relationships_from("root").unwrap().is_empty());
}

#[test]
fn semantic_artifacts_can_be_added_updated_and_removed() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("owner", None)).unwrap();
    storage.upsert_artifact(&artifact("note", "owner")).unwrap();
    let mut updated = artifact("note", "owner");
    updated.title = "updated note".to_string();

    storage.upsert_artifact(&updated).unwrap();
    let removed = storage.remove_artifact("note").unwrap();

    assert!(removed);
    assert!(storage.artifact("note").unwrap().is_none());
}

#[test]
fn settings_can_be_set_and_replaced() {
    let db = DbCore::in_memory().unwrap();
    db.persistent_settings()
        .set_json("app", "theme", &json!("dark"))
        .unwrap();
    db.persistent_settings()
        .set_json("app", "theme", &json!("light"))
        .unwrap();
    db.plugin_settings()
        .set_config("knowledge", true, &json!({"mode": "local"}))
        .unwrap();
    db.plugin_settings()
        .set_config("knowledge", false, &json!({"mode": "remote"}))
        .unwrap();

    let setting = db
        .persistent_settings()
        .get_json("app", "theme")
        .unwrap()
        .unwrap();
    let plugin = db
        .plugin_settings()
        .plugin_settings("knowledge")
        .unwrap()
        .unwrap();
    assert_eq!(setting.value, json!("light"));
    assert!(!plugin.enabled);
    assert_eq!(plugin.config, json!({"mode": "remote"}));
}

#[test]
fn large_artifact_updates_replace_blob_content() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("owner", None)).unwrap();
    let first_content = "first large body ".repeat(700).into_bytes();
    let second_content = "second large body ".repeat(700).into_bytes();

    storage
        .upsert_artifact_content(
            &artifact("large-note", "owner"),
            "text/plain",
            &first_content,
        )
        .unwrap();
    storage
        .upsert_artifact_content(
            &artifact("large-note", "owner"),
            "text/plain",
            &second_content,
        )
        .unwrap();

    let stored = storage.artifact("large-note").unwrap().unwrap();
    let blob = db
        .artifact_blobs()
        .blob(stored.content_ref.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(blob.content, second_content);
    assert!(
        stored
            .searchable_text
            .as_deref()
            .unwrap()
            .contains("second large body")
    );
}

#[test]
fn artifact_blob_replacement_deletes_previous_payload() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("owner", None)).unwrap();
    let large_content = "large body ".repeat(900).into_bytes();

    storage
        .upsert_artifact_content(&artifact("note", "owner"), "text/plain", &large_content)
        .unwrap();
    let old_ref = storage
        .artifact("note")
        .unwrap()
        .unwrap()
        .content_ref
        .unwrap();
    storage
        .upsert_artifact_content(&artifact("note", "owner"), "text/plain", b"small body")
        .unwrap();

    let stored = storage.artifact("note").unwrap().unwrap();
    assert_eq!(stored.content.as_deref(), Some("small body"));
    let new_ref = stored.content_ref.as_deref().unwrap();
    assert_ne!(new_ref, old_ref);
    assert!(db.artifact_blobs().blob(&old_ref).unwrap().is_none());
    assert_eq!(
        db.artifact_blobs().blob(new_ref).unwrap().unwrap().content,
        b"small body"
    );
}

#[test]
fn large_artifact_blob_is_deleted_when_artifact_is_removed() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage.upsert_element(&element("owner", None)).unwrap();
    let large_content = "large body ".repeat(900).into_bytes();
    storage
        .upsert_artifact_content(&artifact("note", "owner"), "text/plain", &large_content)
        .unwrap();
    let content_ref = storage
        .artifact("note")
        .unwrap()
        .unwrap()
        .content_ref
        .unwrap();

    assert!(storage.remove_artifact("note").unwrap());

    assert!(storage.artifact("note").unwrap().is_none());
    assert!(db.artifact_blobs().blob(&content_ref).unwrap().is_none());
}

#[test]
fn minor_semantic_structure_changes_transform_to_existing_identity() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[indexed_element(
                "old-fn",
                "calculate",
                "src/original.rs",
                "fp1:0000000000000001:old-body",
            )],
            &[],
        )
        .unwrap();
    storage
        .upsert_artifact(&artifact("note", "old-fn"))
        .unwrap();
    storage.sync_semantic_structure("/repo", &[], &[]).unwrap();

    let report = storage
        .sync_semantic_structure(
            "/repo",
            &[indexed_element(
                "new-fn",
                "calculate",
                "src/moved.rs",
                "fp1:0000000000000003:new-body",
            )],
            &[],
        )
        .unwrap();

    let transformed = storage.element("old-fn").unwrap().unwrap();
    assert_eq!(report.identities_reused, 1);
    assert_eq!(transformed.path, "src/moved.rs");
    assert_eq!(transformed.lifecycle, "active");
    assert_eq!(
        storage
            .artifact("note")
            .unwrap()
            .unwrap()
            .semantic_element_id,
        "old-fn"
    );
    assert!(storage.element("new-fn").unwrap().is_none());
}

fn element(id: &str, parent: Option<&str>) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: format!("src/{id}.rs"),
        element_kind: "file".to_string(),
        name: id.to_string(),
        parent_element_id: parent.map(str::to_string),
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn indexed_element(id: &str, name: &str, path: &str, fingerprint: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: path.to_string(),
        element_kind: "function".to_string(),
        name: name.to_string(),
        parent_element_id: None,
        content_fingerprint: Some(fingerprint.to_string()),
        start_line: Some(10),
        end_line: Some(12),
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact(id: &str, element_id: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.to_string(),
        semantic_element_id: element_id.to_string(),
        artifact_kind: "definition".to_string(),
        title: id.to_string(),
        content_ref: None,
        content: None,
        searchable_text: None,
        content_size_bytes: None,
        metadata: json!({}),
        dependencies: vec![],
    }
}

fn relationship(source_id: &str, target_id: &str) -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/repo".to_string(),
        source_element_id: source_id.to_string(),
        target_element_id: target_id.to_string(),
        relationship_kind: "contains".to_string(),
        label: "contains".to_string(),
        metadata: json!({}),
    }
}
