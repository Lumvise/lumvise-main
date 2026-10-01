use lumvise_db_core::{
    ChangeDisposition, ChangeHookScope, DbCore, SemanticArtifact, SemanticElement,
};
use std::collections::BTreeSet;

fn element(project_root: &str, id: &str, kind: &str) -> SemanticElement {
    SemanticElement {
        project_root: project_root.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: format!("src/{id}.rs"),
        element_kind: kind.into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
    }
}

fn scope(project_root: &str, kinds: &[&str]) -> ChangeHookScope {
    ChangeHookScope {
        project_root: project_root.into(),
        entity_kinds: kinds
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect::<BTreeSet<_>>(),
    }
}

#[test]
fn coalesces_redirties_and_advances_only_on_ack() {
    let db = DbCore::in_memory().unwrap();
    let hooks = db.change_hooks();
    hooks.register("index", scope("/repo", &["file"])).unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&element("/repo", "a", "file"))
        .unwrap();
    storage
        .upsert_element(&element("/repo", "a", "file"))
        .unwrap();
    storage
        .upsert_element(&element("/repo", "b", "file"))
        .unwrap();

    let first = hooks.dirty_batch("index", 10).unwrap();
    assert_eq!(first.changed.len(), 2);
    assert_eq!(first.changed[0].element_id, "a");
    assert_eq!(first.changed[0].revision, 2);
    assert_eq!(first.changed[1].element_id, "b");
    assert_eq!(hooks.dirty_batch("index", 10).unwrap(), first);
    assert!(hooks.acknowledge(&first).unwrap());
    assert!(hooks.acknowledge(&first).unwrap());

    storage
        .upsert_element(&element("/repo", "a", "file"))
        .unwrap();
    let redirty = hooks.dirty_batch("index", 10).unwrap();
    assert_eq!(redirty.changed.len(), 1);
    assert_eq!(redirty.changed[0].element_id, "a");
    assert_eq!(redirty.changed[0].disposition, ChangeDisposition::Upserted);
}

#[test]
fn scopes_warm_batches_and_reopen_cold_scan() {
    let file = tempfile::NamedTempFile::new().unwrap();
    {
        let db = DbCore::open(file.path()).unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("/one", "one-file", "file"))
            .unwrap();
        storage
            .upsert_element(&element("/one", "one-dir", "directory"))
            .unwrap();
        storage
            .upsert_element(&element("/two", "two-file", "file"))
            .unwrap();
        let hooks = db.change_hooks();
        hooks
            .register("one-files", scope("/one", &["file"]))
            .unwrap();
        let batch = hooks.dirty_batch("one-files", 10).unwrap();
        assert_eq!(
            batch
                .changed
                .iter()
                .map(|change| change.element_id.as_str())
                .collect::<Vec<_>>(),
            vec!["one-file"]
        );
        assert!(hooks.acknowledge(&batch).unwrap());
    }
    let db = DbCore::open(file.path()).unwrap();
    let hooks = db.change_hooks();
    assert_eq!(hooks.registrations().unwrap()[0].watermark, 3);
    let empty = hooks.dirty_batch("one-files", 10).unwrap();
    assert!(empty.changed.is_empty());
    let all = hooks.changes_since(&scope("/one", &[]), 0, 10).unwrap();
    assert_eq!(all.changed.len(), 2);
}

#[test]
fn delivers_tombstone_then_collects_it_at_global_floor() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .upsert_element(&element("/repo", "deleted", "file"))
        .unwrap();
    let hooks = db.change_hooks();
    hooks.register("first", scope("/repo", &[])).unwrap();
    hooks.register("second", scope("/repo", &[])).unwrap();
    assert!(storage.remove_element("deleted").unwrap());

    for name in ["first", "second"] {
        let batch = hooks.dirty_batch(name, 10).unwrap();
        assert_eq!(
            batch.changed.last().unwrap().disposition,
            ChangeDisposition::Removal
        );
        assert!(hooks.acknowledge(&batch).unwrap());
    }
    assert_eq!(hooks.gc_tombstones().unwrap(), 1);
    assert!(storage.element("deleted").unwrap().is_none());
}

#[test]
fn registration_replacement_rotates_ack_and_deregistration_releases_floor() {
    let db = DbCore::in_memory().unwrap();
    let hooks = db.change_hooks();
    hooks.register("hook", scope("/repo", &["file"])).unwrap();
    db.storage_manager()
        .semantic_storage()
        .upsert_element(&element("/repo", "a", "file"))
        .unwrap();
    let stale = hooks.dirty_batch("hook", 10).unwrap();
    hooks
        .register("hook", scope("/repo", &["directory"]))
        .unwrap();
    assert!(!hooks.acknowledge(&stale).unwrap());
    assert!(hooks.deregister("hook").unwrap());
    assert!(hooks.registrations().unwrap().is_empty());
}

#[test]
fn artifact_changes_redirty_the_owner_across_a_cold_restart() {
    let file = tempfile::NamedTempFile::new().unwrap();
    {
        let db = DbCore::open(file.path()).unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .upsert_element(&element("/repo", "owner", "file"))
            .unwrap();
        let hooks = db.change_hooks();
        hooks.register("obsidian", scope("/repo", &[])).unwrap();
        let baseline = hooks.dirty_batch("obsidian", 10).unwrap();
        assert!(hooks.acknowledge(&baseline).unwrap());
        storage
            .upsert_artifact(&SemanticArtifact {
                artifact_id: "artifact".into(),
                semantic_element_id: "owner".into(),
                artifact_kind: "knowledge".into(),
                title: "Artifact".into(),
                content_ref: None,
                content: Some("body".into()),
                searchable_text: Some("body".into()),
                content_size_bytes: Some(4),
                metadata: serde_json::json!({}),
                dependencies: vec![],
            })
            .unwrap();
        let batch = hooks.dirty_batch("obsidian", 10).unwrap();
        assert_eq!(batch.changed.len(), 1);
        assert_eq!(batch.changed[0].element_id, "owner");
        assert_eq!(batch.changed[0].disposition, ChangeDisposition::Upserted);
    }
    let db = DbCore::open(file.path()).unwrap();
    let batch = db.change_hooks().dirty_batch("obsidian", 10).unwrap();
    assert_eq!(batch.changed.len(), 1);
    assert_eq!(batch.changed[0].element_id, "owner");
    db.storage_manager()
        .semantic_storage()
        .remove_artifact("artifact")
        .unwrap();
    let batch = db.change_hooks().dirty_batch("obsidian", 10).unwrap();
    assert_eq!(batch.changed.len(), 1);
    assert_eq!(batch.changed[0].element_id, "owner");
    assert_eq!(batch.changed[0].disposition, ChangeDisposition::Upserted);
}

#[test]
fn changes_since_revision_page_reports_the_published_head() {
    use lumvise_db_core::{
        LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult,
    };
    use lumvise_resource_routing::InvocationControl;

    let persistence = LocalPersistence::in_memory().unwrap();
    let control = InvocationControl::sixty_seconds();
    let result = SemanticPersistence::execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("/repo", "a", "file"), element("/repo", "b", "file")],
            relationships: vec![],
        },
        &control,
    )
    .unwrap();
    assert!(matches!(result, SemanticResult::SyncStructure(_)));
    let SemanticResult::SemanticRevision { commit_version } =
        SemanticPersistence::execute(&persistence, SemanticOperation::SemanticRevision, &control)
            .unwrap()
    else {
        panic!("unexpected semantic result");
    };

    let result = SemanticPersistence::execute(
        &persistence,
        SemanticOperation::ChangesSinceRevision {
            scope: scope("/repo", &[]),
            after_revision: 0,
            limit: 100,
        },
        &control,
    )
    .unwrap();
    let SemanticResult::ChangesSinceRevision(page) = result else {
        panic!("unexpected semantic result");
    };
    assert_eq!(page.head_revision, commit_version);
    assert_eq!(page.target_revision, commit_version);
    assert!(!page.has_more);
}
