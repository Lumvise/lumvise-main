use lumvise_db_core::{
    DbCore, SemanticArtifact, SemanticElement, SemanticPartition, SemanticRelationship,
};
use serde_json::json;

#[test]
fn settings_and_plugin_settings_survive_reopen() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();

    db.persistent_settings()
        .set_json("app", "theme", &json!("dark"))
        .unwrap();
    db.plugin_settings()
        .set_config("knowledge", true, &json!({"mode": "local"}))
        .unwrap();
    drop(db);

    let reopened = DbCore::open(temp.path()).unwrap();
    let setting = reopened
        .persistent_settings()
        .get_json("app", "theme")
        .unwrap()
        .unwrap();
    let plugin = reopened
        .plugin_settings()
        .plugin_settings("knowledge")
        .unwrap()
        .unwrap();

    assert_eq!(setting.value, json!("dark"));
    assert!(plugin.enabled);
    assert_eq!(plugin.config, json!({"mode": "local"}));
}

#[test]
fn sql_schema_contains_only_sql_owned_database_tables() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    drop(db);

    let conn = rusqlite::Connection::open(temp.path()).unwrap();
    let table_names = sql_table_names(&conn);

    assert!(table_names.contains(&"persistent_settings".to_string()));
    assert!(table_names.contains(&"plugin_settings".to_string()));
    assert!(table_names.contains(&"artifact_blobs".to_string()));
    assert!(!table_names.contains(&"semantic_elements".to_string()));
    assert!(!table_names.contains(&"semantic_relationships".to_string()));
    assert!(!table_names.contains(&"semantic_artifacts".to_string()));
    assert!(!table_names.contains(&"trigger_registrations".to_string()));
}

#[test]
fn fresh_schema_has_no_snapshot_lease_or_blob_retention_state() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    drop(db);
    let conn = rusqlite::Connection::open(temp.path()).unwrap();
    let table_names = sql_table_names(&conn);
    assert!(!table_names.contains(&"snapshot_leases".to_string()));
    let columns = conn
        .prepare("PRAGMA table_info(artifact_blobs)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(!columns.iter().any(|column| column == "deleted"));
    assert!(!columns.iter().any(|column| column == "retained_until"));
}

#[test]
fn grafeo_semantic_graph_survives_reopen() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    db.storage_manager()
        .semantic_storage()
        .upsert_element(&indexed_element(
            "persisted",
            "persisted",
            None,
            "fp1:0000000000000001:persisted",
        ))
        .unwrap();
    drop(db);

    let reopened = DbCore::open(temp.path()).unwrap();
    let element = reopened
        .storage_manager()
        .semantic_storage()
        .element("persisted")
        .unwrap()
        .unwrap();

    assert_eq!(element.name, "persisted");
    assert_eq!(element.lifecycle, "active");
}

#[test]
fn project_snapshot_extracts_complete_scoped_graph_with_fresh_revision() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[element("file:a", None), element("fn:a", Some("file:a"))],
            &[relationship("file:a", "fn:a")],
        )
        .unwrap();
    let mut note = artifact("note-a", "fn:a");
    note.metadata = json!({"knowledge": {"type": "definition"}});
    storage.upsert_artifact(&note).unwrap();

    let snapshot = storage
        .project_snapshot(
            &lumvise_db_core::ProjectSnapshotScope::ProjectRoot("/repo".into()),
            Some("knowledge"),
        )
        .unwrap();
    assert_eq!(snapshot.project_root, "/repo");
    assert_eq!(snapshot.elements.len(), 2);
    assert_eq!(snapshot.relationships.len(), 1);
    assert_eq!(snapshot.artifacts, vec![note]);
    assert!(snapshot.commit_version > 0);
    assert!(!snapshot.published_at.is_empty());

    let element_scoped = storage
        .project_snapshot(
            &lumvise_db_core::ProjectSnapshotScope::SemanticElement("fn:a".into()),
            Some("knowledge"),
        )
        .unwrap();
    assert_eq!(element_scoped.commit_version, snapshot.commit_version);
    assert_eq!(element_scoped.project_root, snapshot.project_root);
    assert_eq!(element_scoped.elements, snapshot.elements);
}

#[test]
fn small_artifact_content_persists_raw_blob_and_searchable_graph_text() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&indexed_element(
            "inline-owner",
            "owner",
            None,
            "fp1:0000000000000001:inline-owner",
        ))
        .unwrap();

    storage
        .upsert_artifact_content(
            &artifact("inline-note", "inline-owner"),
            "text/plain",
            b"small searchable note",
        )
        .unwrap();

    let stored = storage.artifact("inline-note").unwrap().unwrap();
    assert_eq!(stored.content.as_deref(), Some("small searchable note"));
    assert_eq!(
        stored.searchable_text.as_deref(),
        Some("small searchable note")
    );
    let content_ref = stored.content_ref.as_deref().unwrap();
    assert_eq!(
        db.artifact_blobs()
            .blob(content_ref)
            .unwrap()
            .unwrap()
            .content,
        b"small searchable note"
    );
}

#[test]
fn large_artifact_content_spills_to_sql_and_keeps_searchable_graph_text() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&indexed_element(
            "large-owner",
            "owner",
            None,
            "fp1:0000000000000001:large-owner",
        ))
        .unwrap();
    let large_content = "searchable-prefix ".repeat(700).into_bytes();

    storage
        .upsert_artifact_content(
            &artifact("large-note", "large-owner"),
            "text/plain",
            &large_content,
        )
        .unwrap();

    let stored = storage.artifact("large-note").unwrap().unwrap();
    let content_ref = stored.content_ref.as_deref().unwrap();
    let blob = db.artifact_blobs().blob(content_ref).unwrap().unwrap();

    assert_eq!(
        stored.content.as_deref(),
        std::str::from_utf8(&large_content).ok()
    );
    assert!(
        stored
            .searchable_text
            .as_deref()
            .unwrap()
            .contains("searchable-prefix")
    );
    assert_eq!(stored.content_size_bytes, Some(large_content.len()));
    assert_eq!(blob.content, large_content);
}

#[test]
fn missing_blob_falls_back_to_searchable_text_for_small_content() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&indexed_element(
            "damaged-owner",
            "owner",
            None,
            "fp1:0000000000000001:damaged-owner",
        ))
        .unwrap();
    let content = "damaged knowledge body ".repeat(40);

    storage
        .upsert_artifact_content(
            &artifact("damaged-note", "damaged-owner"),
            "text/plain",
            content.as_bytes(),
        )
        .unwrap();

    let content_ref = storage
        .artifact("damaged-note")
        .unwrap()
        .unwrap()
        .content_ref
        .unwrap();
    assert!(db.artifact_blobs().delete_blob(&content_ref).unwrap());

    // A single read and the project listing must both survive the loss.
    let stored = storage.artifact("damaged-note").unwrap().unwrap();
    assert_eq!(stored.content.as_deref(), Some(content.as_str()));
    let listed = storage.project_artifacts("/repo", None).unwrap();
    assert!(listed.iter().any(|note| note.artifact_id == "damaged-note"
        && note.content.as_deref() == Some(content.as_str())));
}

#[test]
fn missing_blob_over_searchable_limit_stays_unhydrated() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&indexed_element(
            "oversized-owner",
            "owner",
            None,
            "fp1:0000000000000001:oversized-owner",
        ))
        .unwrap();
    let oversized = "searchable-prefix ".repeat(700);

    storage
        .upsert_artifact_content(
            &artifact("oversized-note", "oversized-owner"),
            "text/plain",
            oversized.as_bytes(),
        )
        .unwrap();

    let content_ref = storage
        .artifact("oversized-note")
        .unwrap()
        .unwrap()
        .content_ref
        .unwrap();
    assert!(db.artifact_blobs().delete_blob(&content_ref).unwrap());

    // No complete copy remains on the node; hydration must not fabricate a
    // truncated value.
    let stored = storage.artifact("oversized-note").unwrap().unwrap();
    assert_eq!(stored.content, None);
    let searchable = stored.searchable_text.unwrap();
    assert_eq!(searchable.chars().count(), 4096);
    assert!(searchable.starts_with("searchable-prefix "));
}

#[test]
fn large_artifact_sql_spill_survives_reopen_with_grafeo_link() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&indexed_element(
            "durable-owner",
            "owner",
            None,
            "fp1:0000000000000001:durable-owner",
        ))
        .unwrap();
    let large_content = "durable searchable body ".repeat(500).into_bytes();
    storage
        .upsert_artifact_content(
            &artifact("durable-note", "durable-owner"),
            "text/plain",
            &large_content,
        )
        .unwrap();
    drop(db);

    let reopened = DbCore::open(temp.path()).unwrap();
    let stored = reopened
        .storage_manager()
        .semantic_storage()
        .artifact("durable-note")
        .unwrap()
        .unwrap();
    let blob = reopened
        .artifact_blobs()
        .blob(stored.content_ref.as_deref().unwrap())
        .unwrap()
        .unwrap();

    assert!(
        stored
            .searchable_text
            .as_deref()
            .unwrap()
            .contains("durable searchable")
    );
    assert_eq!(
        stored.content.as_deref(),
        std::str::from_utf8(&large_content).ok()
    );
    assert_eq!(blob.content, large_content);
}

#[test]
fn artifact_blob_round_trips_binary_content() {
    let db = DbCore::in_memory().unwrap();

    db.artifact_blobs()
        .put_blob(
            "blob://artifact/a",
            "artifact-a",
            "application/octet-stream",
            &[0, 1, 2],
        )
        .unwrap();

    let blob = db
        .artifact_blobs()
        .blob("blob://artifact/a")
        .unwrap()
        .unwrap();
    assert_eq!(blob.content, vec![0, 1, 2]);
    assert_eq!(blob.media_type, "application/octet-stream");
}

#[test]
fn semantic_storage_records_elements_artifacts_relationships() {
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

    assert_eq!(storage.element("child").unwrap().unwrap().name, "child");
    assert_eq!(storage.artifact("note").unwrap().unwrap().title, "note");
    assert_eq!(storage.relationships_from("root").unwrap().len(), 1);
}

#[test]
fn relationship_delta_preserves_artifact_edge_and_distinct_relationship_labels() {
    let (db, retained) = graph_with_distinct_relationship_labels();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[element("root", None), element("child", None)],
            &[retained],
        )
        .unwrap();
    assert_relationship_delta(&storage);
}

fn assert_relationship_delta(storage: &lumvise_db_core::SemanticStorage<'_>) {
    let relationships = storage.relationships_from("root").unwrap();
    assert_eq!(relationships.len(), 1);
    assert_eq!(relationships[0].label, "retained");
    assert_eq!(relationships[0].metadata, json!({"revision": 2}));
    assert_eq!(
        storage.artifact("note").unwrap().unwrap().artifact_id,
        "note"
    );
}

fn graph_with_distinct_relationship_labels() -> (DbCore, SemanticRelationship) {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let elements = [element("root", None), element("child", None)];
    let mut retained = relationship("root", "child");
    retained.label = "retained".to_string();
    let mut removed = retained.clone();
    removed.label = "removed".to_string();
    storage
        .sync_semantic_structure("/repo", &elements, &[retained.clone(), removed])
        .unwrap();
    storage.upsert_artifact(&artifact("note", "root")).unwrap();
    retained.metadata = json!({"revision": 2});
    (db, retained)
}

#[test]
fn sync_reuses_exact_fingerprint_identity_and_remaps_relationships() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[indexed_element(
                "old-fn",
                "calculate",
                None,
                "fp1:0000000000000001:stable-body",
            )],
            &[],
        )
        .unwrap();
    storage
        .upsert_artifact(&artifact("note", "old-fn"))
        .unwrap();

    let report = storage
        .sync_semantic_structure(
            "/repo",
            &[
                indexed_element(
                    "new-fn",
                    "compute_total",
                    None,
                    "fp1:0000000000000001:stable-body",
                ),
                indexed_element("helper", "helper", None, "fp1:0000000000000002:helper-body"),
            ],
            &[relationship("new-fn", "helper")],
        )
        .unwrap();

    let rebound = storage.element("old-fn").unwrap().unwrap();
    assert_eq!(report.identities_reused, 1);
    assert_eq!(rebound.name, "compute_total");
    assert_eq!(rebound.path, "src/new-fn.rs");
    assert!(storage.element("new-fn").unwrap().is_none());
    assert_eq!(storage.relationships_from("old-fn").unwrap().len(), 1);
    assert_eq!(
        storage
            .artifact("note")
            .unwrap()
            .unwrap()
            .semantic_element_id,
        "old-fn"
    );
}

#[test]
fn sync_reuses_similar_fp1_identity_with_match_evidence() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[indexed_element(
                "old-fn",
                "calculate",
                None,
                "fp1:0000000000000001:old-body",
            )],
            &[],
        )
        .unwrap();
    storage.sync_semantic_structure("/repo", &[], &[]).unwrap();

    let report = storage
        .sync_semantic_structure(
            "/repo",
            &[indexed_element(
                "new-fn",
                "calculate",
                None,
                "fp1:0000000000000003:new-body",
            )],
            &[],
        )
        .unwrap();

    let evidence = storage
        .element("old-fn")
        .unwrap()
        .unwrap()
        .match_evidence
        .unwrap();
    assert_eq!(report.identities_reused, 1);
    assert_eq!(evidence.simhash_distance, Some(1));
    assert_eq!(evidence.match_confidence, 90);
    assert_eq!(evidence.precaution.as_deref(), Some("similar_rebind"));
}

#[test]
fn sync_rejects_fuzzy_reuse_when_parent_context_differs() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                indexed_element("old-parent", "ServiceA", None, "fp1:0000000000000001:pa"),
                indexed_element(
                    "old-child",
                    "calculate",
                    Some("old-parent"),
                    "fp1:0000000000000000:old",
                ),
            ],
            &[],
        )
        .unwrap();
    storage.sync_semantic_structure("/repo", &[], &[]).unwrap();

    let report = storage
        .sync_semantic_structure(
            "/repo",
            &[
                indexed_element("new-parent", "ServiceB", None, "fp1:ffffffffffffffff:pb"),
                indexed_element(
                    "new-child",
                    "calculate",
                    Some("new-parent"),
                    "fp1:0000000000000001:new",
                ),
            ],
            &[],
        )
        .unwrap();

    assert_eq!(report.identities_reused, 0);
    assert!(
        storage.element("old-child").unwrap().is_none(),
        "non-reused tombstones are hidden from active element reads"
    );
    assert_eq!(
        storage.element("new-child").unwrap().unwrap().lifecycle,
        "active"
    );
}

#[test]
fn sync_reuses_child_when_parent_identity_is_remapped() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .sync_semantic_structure(
            "/repo",
            &[
                indexed_element(
                    "old-parent",
                    "Service",
                    None,
                    "fp1:0000000000000010:stable-parent",
                ),
                indexed_element(
                    "old-child",
                    "calculate",
                    Some("old-parent"),
                    "fp1:0000000000000000:old",
                ),
            ],
            &[],
        )
        .unwrap();
    storage.sync_semantic_structure("/repo", &[], &[]).unwrap();

    let report = storage
        .sync_semantic_structure(
            "/repo",
            &[
                indexed_element(
                    "new-parent",
                    "Service",
                    None,
                    "fp1:0000000000000010:stable-parent",
                ),
                indexed_element(
                    "new-child",
                    "calculate",
                    Some("new-parent"),
                    "fp1:0000000000000001:new",
                ),
            ],
            &[],
        )
        .unwrap();

    assert_eq!(report.identities_reused, 2);
    assert!(storage.element("old-child").unwrap().is_some());
    assert!(storage.element("new-child").unwrap().is_none());
}

#[test]
fn semantic_storage_rejects_missing_references() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();

    let artifact_error = storage
        .upsert_artifact(&artifact("note", "missing"))
        .unwrap_err();
    let relationship_error = storage
        .link_elements(&relationship("source-missing", "target-missing"))
        .unwrap_err();

    assert!(artifact_error.to_string().contains("missing"));
    assert!(
        artifact_error
            .to_string()
            .contains("existing semantic element id")
    );
    assert!(relationship_error.to_string().contains("source-missing"));
}

#[test]
fn published_graph_commit_advances_dispatcher_wake_generation() {
    let db = DbCore::in_memory().unwrap();
    let before = db.storage_change_generation();

    db.storage_manager()
        .semantic_storage()
        .upsert_element(&element("wake-element", None))
        .unwrap();

    assert!(db.storage_change_generation() > before);
}

#[test]
fn semantic_partition_sync_preserves_unrelated_graph_regions() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let initial = vec![
        path_element("file:a", "src/a.rs", "file", None),
        path_element("fn:a", "src/a.rs", "function", Some("file:a")),
        path_element("file:b", "src/b.rs", "file", None),
        path_element("fn:b", "src/b.rs", "function", Some("file:b")),
    ];
    let initial_relationships = vec![
        relationship("file:a", "fn:a"),
        relationship("file:b", "fn:b"),
        call_relationship("fn:a", "fn:b"),
    ];
    storage
        .sync_semantic_structure("/repo", &initial, &initial_relationships)
        .unwrap();

    let partition = SemanticPartition {
        project_root: "/repo".to_string(),
        replace_paths: vec!["src/a.rs".to_string()],
    };
    let updated = vec![
        path_element("file:a", "src/a.rs", "file", None),
        path_element("fn:a2", "src/a.rs", "function", Some("file:a")),
    ];
    storage
        .sync_semantic_partition(&partition, &updated, &[relationship("file:a", "fn:a2")])
        .unwrap();

    let elements = storage.elements_for_project("/repo").unwrap();
    assert!(!active_element_exists(&elements, "fn:a"));
    assert_eq!(storage.relationships_from("file:b").unwrap().len(), 1);
    assert_eq!(storage.relationships_from("fn:a").unwrap().len(), 0);
}

#[test]
fn empty_required_values_are_rejected() {
    let db = DbCore::in_memory().unwrap();
    let error = db
        .persistent_settings()
        .set_json("", "theme", &json!("dark"))
        .unwrap_err();

    assert!(error.to_string().contains("invalid value"));
    assert!(error.to_string().contains("non-empty setting scope"));
}

#[test]
fn unversioned_content_fingerprint_is_rejected() {
    let db = DbCore::in_memory().unwrap();
    let error = db
        .storage_manager()
        .semantic_storage()
        .upsert_element(&indexed_element("old", "old", None, "plain-hash"))
        .unwrap_err();

    assert!(error.to_string().contains("plain-hash"));
    assert!(error.to_string().contains("versioned fp1"));
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

fn path_element(id: &str, path: &str, element_kind: &str, parent: Option<&str>) -> SemanticElement {
    SemanticElement {
        path: path.to_string(),
        element_kind: element_kind.to_string(),
        ..element(id, parent)
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

fn call_relationship(source_id: &str, target_id: &str) -> SemanticRelationship {
    SemanticRelationship {
        relationship_kind: "calls".to_string(),
        label: "calls".to_string(),
        ..relationship(source_id, target_id)
    }
}

fn active_element_exists(elements: &[SemanticElement], element_id: &str) -> bool {
    elements
        .iter()
        .any(|element| element.semantic_element_id == element_id && element.lifecycle == "active")
}

fn sql_table_names(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn indexed_element(
    id: &str,
    name: &str,
    parent: Option<&str>,
    fingerprint: &str,
) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: format!("src/{id}.rs"),
        element_kind: "function".to_string(),
        name: name.to_string(),
        parent_element_id: parent.map(str::to_string),
        content_fingerprint: Some(fingerprint.to_string()),
        start_line: Some(10),
        end_line: Some(12),
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}
