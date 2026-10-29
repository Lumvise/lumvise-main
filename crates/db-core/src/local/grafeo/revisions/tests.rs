//! Real graph/SQL collection with the runtime's injected clock; no sleeps or public test hooks.
use super::*;
use crate::local::clock::Clock;
use crate::local::runtime::DbCore;
use crate::local::sql::artifact_blobs::ArtifactBlobRepository;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

struct RevisionTestClock(AtomicI64);
impl Clock for RevisionTestClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.0.load(Ordering::SeqCst)).unwrap()
    }
}

fn element(revision: i64) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: "/revisions".into(),
        semantic_element_id: "source".into(),
        semantic_source_id: "source".into(),
        path: "main.rs".into(),
        element_kind: "function".into(),
        name: "work".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({ "revision": revision }),
    }
}

fn note(id: &str, content: &str) -> SemanticArtifactRevisionWrite {
    SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: SemanticArtifact {
            artifact_id: id.into(),
            semantic_element_id: "source".into(),
            artifact_kind: "annotation".into(),
            title: "Note".into(),
            content_ref: None,
            content: Some(content.into()),
            searchable_text: None,
            content_size_bytes: None,
            dependencies: vec![],
            metadata: json!({}),
        },
        media_type: "text/plain".into(),
    }
}

fn request(
    storage: &SemanticStorage<'_>,
    operation_id: &str,
    revision: i64,
    artifact: SemanticArtifactRevisionWrite,
) -> SemanticRevisionPublication {
    SemanticRevisionPublication {
        operation_id: operation_id.into(),
        producer_base: None,
        project_root: "/revisions".into(),
        project_id: crate::local::sql::project_lineages::ProjectLineageRepository::new(
            storage.conn,
        )
        .get_or_create("/revisions")
        .unwrap()
        .project_id,
        selection_id: "working".into(),
        source_context: "main".into(),
        expected: storage
            .revision_selection("/revisions", "working")
            .unwrap()
            .map_or_else(SemanticSelectionExpectation::default, |head| {
                head.expectation
            }),
        structure: Some(SemanticRevisionStructure {
            partition: None,
            elements: vec![element(revision)],
            relationships: vec![],
        }),
        artifacts: vec![artifact],
        removed_artifact_ids: vec![],
        source_anchor: None,
    }
}

#[test]
fn a_blob_collision_rolls_back_every_inline_body_before_graph_mutation() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut publication = request(&storage, "atomic-bodies", 1, note("first", "first bytes"));
    publication.artifacts.push(note("second", "second bytes"));
    let first = publication.artifacts[0]
        .stored_artifact(&publication.project_id)
        .unwrap()
        .content_ref
        .unwrap();
    let second = publication.artifacts[1]
        .stored_artifact(&publication.project_id)
        .unwrap()
        .content_ref
        .unwrap();
    let blobs = ArtifactBlobRepository::with_clock(storage.conn, storage.clock.clone());
    blobs
        .put_blob(&second, "preexisting", "text/plain", b"collision")
        .unwrap();
    let error = storage.publish_revisions(&publication).unwrap_err();
    assert!(error.to_string().contains(&second));
    assert!(blobs.blob(&first).unwrap().is_none());
    assert_eq!(blobs.blob(&second).unwrap().unwrap().content, b"collision");
    assert!(
        storage
            .revision_selection("/revisions", "working")
            .unwrap()
            .is_none()
    );
    publication.artifacts.truncate(1);
    assert!(matches!(
        storage.publish_revisions(&publication).unwrap(),
        SemanticRevisionPublicationOutcome::Accepted(_)
    ));
}

#[test]
fn a_missing_exact_content_reference_rolls_back_other_inline_bodies() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut publication = request(&storage, "missing-body", 1, note("first", "first bytes"));
    let first = publication.artifacts[0]
        .stored_artifact(&publication.project_id)
        .unwrap()
        .content_ref
        .unwrap();
    let mut missing = note("second", "unused");
    missing.artifact.content = None;
    missing.artifact.content_ref = Some("missing:exact".into());
    publication.artifacts.push(missing);
    let error = storage.publish_revisions(&publication).unwrap_err();
    assert!(error.to_string().contains("missing:exact"));
    assert!(
        ArtifactBlobRepository::with_clock(storage.conn, storage.clock.clone())
            .blob(&first)
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .revision_selection("/revisions", "working")
            .unwrap()
            .is_none()
    );
    publication.artifacts.truncate(1);
    assert!(matches!(
        storage.publish_revisions(&publication).unwrap(),
        SemanticRevisionPublicationOutcome::Accepted(_)
    ));
}

#[test]
fn inherited_note_resaves_reuse_the_body_after_collecting_its_canonical_owner() {
    let clock = Arc::new(RevisionTestClock(AtomicI64::new(1000)));
    let db = DbCore::in_memory_with_clock(clock.clone()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .publish_revisions(&request(&storage, "initial", 1, note("note", "same bytes")))
        .unwrap();
    let mut advance = request(&storage, "advance", 2, note("note", "same bytes"));
    advance.artifacts.clear();
    storage.publish_revisions(&advance).unwrap();
    let current_owner = storage
        .revision_selection("/revisions", "working")
        .unwrap()
        .unwrap()
        .elements[0]
        .semantic_element_id
        .clone();
    clock.0.store(20_000, Ordering::SeqCst);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = storage
        .plan_revision_retention("/revisions", "collect", &policy)
        .unwrap();
    let receipt = storage.collect_revision_retention(&plan, &policy).unwrap();
    assert!(
        receipt
            .pruned_records
            .contains(&SemanticRecordRef::element("source"))
    );
    for owner in ["source", current_owner.as_str()] {
        let mut save = request(
            &storage,
            &format!("resave-{owner}"),
            2,
            note("note", "same bytes"),
        );
        save.structure = None;
        save.artifacts[0].artifact.semantic_element_id = owner.into();
        let SemanticRevisionPublicationOutcome::Accepted(receipt) =
            storage.publish_revisions(&save).unwrap()
        else {
            panic!("expected an unchanged note publication");
        };
        assert_eq!(
            receipt.remaps[0].accepted,
            SemanticRecordRef::artifact("note")
        );
        assert!(receipt.revision_links.is_empty());
        let note = storage.artifact("note").unwrap().unwrap();
        assert_eq!(note.semantic_element_id, "source");
        assert_eq!(note.content.as_deref(), Some("same bytes"));
    }
}

#[test]
fn count_retention_keeps_two_element_and_artifact_revisions_and_replays_the_receipt() {
    let clock = Arc::new(RevisionTestClock(AtomicI64::new(1000)));
    let db = DbCore::in_memory_with_clock(clock.clone()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    for revision in 0..12 {
        clock.0.store((revision + 1) * 1000, Ordering::SeqCst);
        let request = request(
            &storage,
            &format!("version-{revision}"),
            revision,
            note("note", &format!("N{revision}")),
        );
        storage.publish_revisions(&request).unwrap();
    }
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 2 };
    let plan = storage
        .plan_revision_retention("/revisions", "counted", &policy)
        .unwrap();
    assert_eq!(plan.records.len(), 20);
    let receipt = storage.collect_revision_retention(&plan, &policy).unwrap();
    assert_eq!(receipt.pruned_records.len(), 20);
    assert_eq!(receipt.removed_blob_refs.len(), 10);
    assert_eq!(
        storage.collect_revision_retention(&plan, &policy).unwrap(),
        receipt
    );
    let history = storage.revision_history("/revisions").unwrap();
    assert_eq!(
        history
            .records
            .iter()
            .filter(|stored| !matches!(stored.body, SemanticRevisionBody::Pruned))
            .count(),
        4
    );
    let current = storage
        .revision_selection("/revisions", "working")
        .unwrap()
        .unwrap();
    assert_eq!(current.artifacts[0].content.as_deref(), Some("N11"));
    assert_eq!(current.elements.len(), 1);
}

#[test]
fn collection_removes_physical_bodies_and_bytes_but_a_lost_ack_retry_cannot_delete_new_references()
{
    let clock = Arc::new(RevisionTestClock(AtomicI64::new(1000)));
    let db = DbCore::in_memory_with_clock(clock.clone()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    let initial = request(&storage, "initial", 1, note("note", "first body"));
    storage.publish_revisions(&initial).unwrap();
    let old_ref = storage
        .artifact("note")
        .unwrap()
        .unwrap()
        .content_ref
        .unwrap();
    ArtifactBlobRepository::with_clock(storage.conn, clock.clone())
        .put_blob(
            "raw:original-attachment",
            "note",
            "application/octet-stream",
            &[0, 255, 17],
        )
        .unwrap();
    clock.0.store(2000, Ordering::SeqCst);
    storage
        .publish_revisions(&request(&storage, "second", 2, note("note", "second body")))
        .unwrap();
    clock.0.store(20_000, Ordering::SeqCst);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = storage
        .plan_revision_retention("/revisions", "collect", &policy)
        .unwrap();
    assert!(plan.records.contains(&SemanticRecordRef::element("source")));
    assert!(plan.records.contains(&SemanticRecordRef::artifact("note")));
    let receipt = storage.collect_revision_retention(&plan, &policy).unwrap();
    assert!(
        storage
            .conn
            .read_conn()
            .query_row(
                "SELECT COUNT(*) FROM artifact_blobs WHERE content_ref = ?1",
                [&old_ref],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()
            == 0
    );
    db.graph.read(|graph| {
        assert!(
            super::super::graph_rows::node_ids_by_label_and_property(
                graph,
                "SemanticElement",
                "semantic_element_id",
                "source"
            )
            .is_empty()
        );
        assert!(
            super::super::graph_rows::node_ids_by_label_and_property(
                graph,
                "SemanticArtifact",
                "artifact_id",
                "note"
            )
            .is_empty()
        );
    });
    assert!(matches!(
        storage
            .resolve_revision("/revisions", &SemanticRecordRef::element("source"))
            .unwrap()
            .unwrap()
            .body,
        SemanticRevisionBody::Pruned
    ));
    assert_collected_archive_preserves_exact_bytes(&db, &initial);
    let mut reuse = request(&storage, "reuse-content", 2, note("new-note", "first body"));
    reuse.structure = None;
    storage.publish_revisions(&reuse).unwrap();
    let before = storage
        .revision_history("/revisions")
        .unwrap()
        .commit_version;
    assert_eq!(
        storage.collect_revision_retention(&plan, &policy).unwrap(),
        receipt
    );
    assert_eq!(
        storage
            .revision_history("/revisions")
            .unwrap()
            .commit_version,
        before
    );
    assert!(
        ArtifactBlobRepository::with_clock(storage.conn, clock)
            .blob(&old_ref)
            .unwrap()
            .is_some()
    );
}

fn assert_collected_archive_preserves_exact_bytes(
    db: &DbCore,
    original: &SemanticRevisionPublication,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("collected-history.pz");
    let control = lumvise_resource_routing::InvocationControl::sixty_seconds();
    let archive =
        crate::local::project_archive::capture(&db.storage_manager(), "/revisions", &control)
            .unwrap();
    archive.write(&path, &control).unwrap();
    let restored = SemanticArchive::read(&path, "/restored", &control).unwrap();
    let current = &restored.snapshot.artifacts[0];
    let reference = current.content_ref.as_ref().unwrap();
    assert_ne!(current.artifact_id, "note");
    assert_eq!(
        restored
            .artifact_blobs
            .iter()
            .find(|blob| &blob.content_ref == reference)
            .unwrap()
            .content,
        b"second body"
    );
    assert_eq!(
        restored
            .artifact_blobs
            .iter()
            .find(|blob| blob.content_ref == "raw:original-attachment")
            .unwrap()
            .content,
        vec![0, 255, 17]
    );
    let target = DbCore::in_memory().unwrap();
    crate::local::pz::import_semantic_snapshot_controlled(
        &target.storage_manager(),
        "/restored",
        &path,
        &control,
    )
    .unwrap();
    let state = target
        .storage_manager()
        .semantic_storage()
        .revision_history("/restored")
        .unwrap();
    assert_eq!(
        state.selections,
        restored.revision_state.as_ref().unwrap().selections
    );
    assert!(state.commit_version > restored.revision_state.as_ref().unwrap().commit_version);
    assert_eq!(
        target
            .storage_manager()
            .semantic_storage()
            .resolve_revision(
                "/restored",
                &SemanticRecordRef::artifact(&current.artifact_id)
            )
            .unwrap()
            .unwrap()
            .body,
        SemanticRevisionBody::Artifact({
            let mut body = current.clone();
            body.content = Some("second body".into());
            body
        })
    );
    assert!(matches!(
        state
            .resolve(&SemanticRecordRef::artifact("note"))
            .unwrap()
            .body,
        SemanticRevisionBody::Pruned
    ));
    let mut retry = original.clone();
    retry.project_root = "/restored".into();
    for element in &mut retry.structure.as_mut().unwrap().elements {
        element.project_root = "/restored".into();
    }
    let plan = state
        .plan_publication(
            &retry,
            &SemanticRevisionTimestamp {
                unix_ms: 30_000,
                rfc3339: "time".into(),
            },
            state.commit_version + 1,
        )
        .unwrap();
    assert!(matches!(
        plan.outcome,
        SemanticRevisionPublicationOutcome::Accepted(_)
    ));
    assert!(plan.accepted_operation.is_none());
}

#[test]
fn explicit_required_evidence_keeps_its_older_physical_body_after_inherited_attachment_advances() {
    let clock = Arc::new(RevisionTestClock(AtomicI64::new(1000)));
    let db = DbCore::in_memory_with_clock(clock.clone()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut evidence = note("note", "Required evidence");
    evidence
        .artifact
        .dependencies
        .push(lumvise_contracts::ArtifactDependency {
            target: lumvise_contracts::ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: "source".into(),
            },
        });
    storage
        .publish_revisions(&request(&storage, "initial", 1, evidence))
        .unwrap();
    clock.0.store(2000, Ordering::SeqCst);
    let mut update = request(&storage, "second", 2, note("unused", "unused"));
    update.artifacts.clear();
    storage.publish_revisions(&update).unwrap();
    clock.0.store(20_000, Ordering::SeqCst);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = storage
        .plan_revision_retention("/revisions", "collect", &policy)
        .unwrap();
    assert!(!plan.records.contains(&SemanticRecordRef::element("source")));
    storage.collect_revision_retention(&plan, &policy).unwrap();
    assert_eq!(storage.element("source").unwrap(), Some(element(1)));
    db.graph.read(|graph| {
        assert_eq!(
            super::super::graph_rows::node_ids_by_label_and_property(
                graph,
                "SemanticElement",
                "semantic_element_id",
                "source"
            )
            .len(),
            1
        )
    });
}

#[test]
fn inherited_attachment_is_readable_at_the_new_element_after_old_owner_collection() {
    let clock = Arc::new(RevisionTestClock(AtomicI64::new(1000)));
    let db = DbCore::in_memory_with_clock(clock.clone()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    storage
        .publish_revisions(&request(
            &storage,
            "initial",
            1,
            note("note", "Retained note"),
        ))
        .unwrap();
    clock.0.store(2000, Ordering::SeqCst);
    let mut update = request(&storage, "advance", 2, note("unused", "unused"));
    update.artifacts.clear();
    let SemanticRevisionPublicationOutcome::Accepted(receipt) =
        storage.publish_revisions(&update).unwrap()
    else {
        panic!("expected accepted update");
    };
    let ids = std::collections::HashSet::from([receipt.remaps[0].accepted.record_id.clone()]);
    assert_eq!(
        storage
            .artifacts_for_elements(&ids)
            .unwrap()
            .iter()
            .map(|note| note.artifact_id.as_str())
            .collect::<Vec<_>>(),
        vec!["note"]
    );
    clock.0.store(20_000, Ordering::SeqCst);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = storage
        .plan_revision_retention("/revisions", "collect", &policy)
        .unwrap();
    assert!(plan.records.contains(&SemanticRecordRef::element("source")));
    storage.collect_revision_retention(&plan, &policy).unwrap();
    assert_eq!(
        storage.artifacts_for_elements(&ids).unwrap()[0]
            .content
            .as_deref(),
        Some("Retained note")
    );
    assert_eq!(
        storage.project_artifacts("/revisions", None).unwrap().len(),
        1
    );
    storage
        .store_artifact_text_vectors(
            "/revisions",
            &[StoredArtifactTextVector {
                artifact_id: "note".into(),
                semantic_element_id: "source".into(),
                source_text: "Retained note".into(),
                vector: ArtifactTextVector {
                    engine_id: "test".into(),
                    model: None,
                    dimensions: 2,
                    vector: vec![1.0, 0.0],
                    normalized: true,
                    metadata: json!({}),
                },
            }],
        )
        .unwrap();
    let found = storage
        .search_artifact_text_vectors("/revisions", &[1.0, 0.0], 10, "test", None)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "note");
}

#[test]
fn artifact_basis_reads_and_history_delta_do_not_grow_with_unrelated_source() {
    assert_eq!(artifact_basis_size(0), artifact_basis_size(1024));
}

#[test]
fn selection_reads_hydrate_each_required_body_once() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut publication = source_history(&storage, 10);
    publication.artifacts.push(note("note", "exact evidence"));
    storage.publish_revisions(&publication).unwrap();
    let complete = storage.revision_history("/revisions").unwrap();
    let (selected, reads) = selected_state_body_reads(&db, complete.commit_version);
    assert_eq!(reads, selected.records.len());
    assert_eq!(
        selected.snapshot("working", "now").unwrap(),
        complete.snapshot("working", "now").unwrap()
    );
}

fn selected_state_body_reads(db: &DbCore, commit_version: i64) -> (SemanticRevisionState, usize) {
    super::super::revision_rows::take_revision_body_reads();
    let state = db
        .graph
        .stable_read(|graph| {
            super::super::revision_basis::load_selected_revision_state(
                graph,
                "/revisions",
                "working",
                commit_version,
            )
        })
        .unwrap();
    (
        state,
        super::super::revision_rows::take_revision_body_reads(),
    )
}

#[test]
fn scoped_structure_basis_does_not_hydrate_unselected_predecessors() {
    for retained in [0, 10, 100] {
        let db = DbCore::in_memory().unwrap();
        let storage = db.storage_manager().semantic_storage();
        let current = source_history(&storage, retained);
        let complete = storage.revision_history("/revisions").unwrap();
        for paths in [vec![], vec!["main.rs".into()]] {
            let partition = SemanticPartition {
                project_root: "/revisions".into(),
                provider_instance_id: Some("files".into()),
                replace_paths: paths,
            };
            for producer_id in [false, true] {
                let mut publication = current.clone();
                publication.structure.as_mut().unwrap().partition = Some(partition.clone());
                if producer_id {
                    publication.structure.as_mut().unwrap().elements[0].semantic_element_id =
                        "source".into();
                }
                assert_scoped_source_basis(
                    &db,
                    &storage,
                    &publication,
                    &complete,
                    1 + usize::from(producer_id && retained > 0),
                );
            }
        }
    }
}

fn assert_scoped_source_basis(
    db: &DbCore,
    storage: &SemanticStorage<'_>,
    publication: &SemanticRevisionPublication,
    complete: &SemanticRevisionState,
    bodies: usize,
) {
    let partition = publication
        .structure
        .as_ref()
        .unwrap()
        .partition
        .clone()
        .unwrap();
    let query = SemanticRevisionSelectionQuery {
        project_root: "/revisions".into(),
        selection_id: "working".into(),
        structure_coverage: SemanticRevisionStructureCoverage::Partition(partition),
        artifact_ids: vec![],
    };
    let timestamp = SemanticRevisionTimestamp {
        unix_ms: 123,
        rfc3339: "1970-01-01T00:00:00.123Z".into(),
    };
    db.graph.read(|graph| {
        let loaded = super::super::revision_basis::load_publication_basis(
            graph,
            storage.graph,
            publication,
            complete.commit_version,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            loaded.state.records.len(),
            bodies,
            "unselected predecessor bodies entered the basis"
        );
        let basis = SemanticRevisionPublicationBasis {
            structure_coverage: query.structure_coverage.clone(),
            artifact_ids: vec![],
            facts: loaded.state,
        };
        let scoped = basis
            .plan_publication(publication, &timestamp, complete.commit_version + 1)
            .unwrap();
        let full = complete
            .plan_publication(publication, &timestamp, complete.commit_version + 1)
            .unwrap();
        assert_eq!(
            serde_json::to_value(scoped).unwrap(),
            serde_json::to_value(full).unwrap()
        );
        let state = super::super::revision_basis::load_revision_membership_state(
            graph,
            storage.graph,
            &query,
            complete.commit_version,
        )
        .unwrap();
        assert_eq!(
            state.records.len(),
            1,
            "membership hydrated predecessor bodies"
        );
        assert_eq!(
            state.selection_members(&query).unwrap(),
            complete.selection_members(&query).unwrap()
        );
    });
}

fn source_history(storage: &SemanticStorage<'_>, retained: i64) -> SemanticRevisionPublication {
    let mut publication = request(storage, "source-initial", 0, note("unused", "unused"));
    publication.artifacts.clear();
    for revision in 0..=retained {
        publication.operation_id = format!("source-{revision}");
        publication.structure.as_mut().unwrap().elements[0].metadata =
            json!({"provider_instance_id":"files", "revision":revision});
        let outcome = storage.publish_revisions(&publication).unwrap();
        let SemanticRevisionPublicationOutcome::Accepted(receipt) = outcome else {
            panic!("expected accepted source history");
        };
        publication.expected = receipt.accepted_scope;
        publication.structure.as_mut().unwrap().elements[0].semantic_element_id =
            receipt.remaps[0].accepted.record_id.clone();
    }
    publication.operation_id = "source-probe".into();
    publication
}

#[test]
fn scoped_membership_still_loads_the_exact_older_revision_of_a_pin() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let current = source_history(&storage, 10);
    storage
        .select_revisions(&SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: "pin-initial-source".into(),
            project_root: "/revisions".into(),
            project_id: current.project_id,
            selection_id: "old-source".into(),
            source_context: "main".into(),
            expected_generation: 0,
            mode: SemanticSelectionMode::Pinned,
            records: vec![SemanticRecordRef::element("source")],
            connection_ids: vec![],
            source_anchor: None,
        })
        .unwrap();
    let query = source_membership_query("old-source");
    db.graph.read(|graph| {
        let state = super::super::revision_basis::load_revision_membership_state(
            graph,
            storage.graph,
            &query,
            0,
        )
        .unwrap();
        assert_eq!(state.records.len(), 1);
        let SemanticRevisionBody::Element(element) = &state.records[0].body else {
            panic!("expected the exact pinned source body");
        };
        assert_eq!(element.semantic_element_id, "source");
        assert_eq!(element.metadata["revision"], 0);
    });
}

#[test]
fn scoped_membership_keeps_older_endpoints_of_a_selected_connection() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut publication = request(&storage, "first-edge", 1, note("unused", "unused"));
    publication.artifacts.clear();
    publication.structure.as_mut().unwrap().elements[0].metadata["provider_instance_id"] =
        json!("files");
    publication
        .structure
        .as_mut()
        .unwrap()
        .relationships
        .push(SemanticRelationship {
            project_root: "/revisions".into(),
            source_element_id: "source".into(),
            target_element_id: "source".into(),
            relationship_kind: "calls".into(),
            label: "calls".into(),
            metadata: json!({}),
        });
    let SemanticRevisionPublicationOutcome::Accepted(first) =
        storage.publish_revisions(&publication).unwrap()
    else {
        panic!("expected initial relationship");
    };
    publication.operation_id = "newer-without-edge".into();
    publication.expected = first.accepted_scope.clone();
    publication.structure.as_mut().unwrap().elements[0].metadata["revision"] = json!(2);
    publication
        .structure
        .as_mut()
        .unwrap()
        .relationships
        .clear();
    let SemanticRevisionPublicationOutcome::Accepted(newer) =
        storage.publish_revisions(&publication).unwrap()
    else {
        panic!("expected newer source");
    };
    storage
        .select_revisions(&SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: "select-older-connection".into(),
            project_root: "/revisions".into(),
            project_id: publication.project_id,
            selection_id: "working".into(),
            source_context: "main".into(),
            expected_generation: newer.selection.generation,
            mode: SemanticSelectionMode::FollowAccepted,
            records: newer.accepted_scope.records.clone(),
            connection_ids: first.accepted_scope.connection_ids.clone(),
            source_anchor: None,
        })
        .unwrap();
    let query = source_membership_query("working");
    db.graph.read(|graph| {
        let state = super::super::revision_basis::load_revision_membership_state(
            graph,
            storage.graph,
            &query,
            0,
        )
        .unwrap();
        assert_eq!(state.records.len(), 2);
        assert_eq!(state.connections.len(), 1);
        assert_eq!(
            state.connections[0].connection_id,
            first.accepted_scope.connection_ids[0]
        );
        assert!(
            state
                .records
                .iter()
                .any(|stored| stored.record == SemanticRecordRef::element("source"))
        );
    });
    let members = storage.revision_selection_members(&query).unwrap().unwrap();
    assert_eq!(members.expected.records, newer.accepted_scope.records);
    // The edge stays selected evidence, outside the editable scope of its newer primary owner.
    assert!(members.expected.connection_ids.is_empty());
    let selected = storage
        .revision_selection("/revisions", "working")
        .unwrap()
        .unwrap();
    assert_eq!(
        selected.connections[0].connection_id,
        first.accepted_scope.connection_ids[0]
    );
    assert!(
        selected
            .required_records
            .iter()
            .any(|record| record.record == SemanticRecordRef::element("source"))
    );
}

fn source_membership_query(selection: &str) -> SemanticRevisionSelectionQuery {
    SemanticRevisionSelectionQuery {
        project_root: "/revisions".into(),
        selection_id: selection.into(),
        structure_coverage: SemanticRevisionStructureCoverage::Partition(SemanticPartition {
            project_root: "/revisions".into(),
            provider_instance_id: Some("files".into()),
            replace_paths: vec!["main.rs".into()],
        }),
        artifact_ids: vec![],
    }
}

fn artifact_basis_size(unrelated: usize) -> (usize, usize) {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut initial = request(&storage, "initial", 1, note("note", "Body 1"));
    for index in 0..unrelated {
        let mut extra = element(1);
        extra.semantic_element_id = format!("unrelated-{index}");
        extra.path = format!("other/{index}.rs");
        initial.structure.as_mut().unwrap().elements.push(extra);
    }
    storage.publish_revisions(&initial).unwrap();
    let mut update = request(&storage, "artifact-only", 1, note("note", "Body 2"));
    update.structure = None;
    db.graph.read(|graph| {
        let loaded = super::super::revision_basis::load_publication_basis(
            graph,
            storage.graph,
            &update,
            1,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(loaded.state.records.len(), 2);
        assert_eq!(loaded.state.selections[0].records.len(), 2);
    });
    storage.publish_revisions(&update).unwrap();
    let state = storage.revision_history("/revisions").unwrap();
    assert_eq!(
        state.selection("working").unwrap().records.len(),
        unrelated + 2
    );
    let delta = state
        .selection_history
        .iter()
        .max_by_key(|delta| delta.selection.generation)
        .unwrap();
    (delta.added_records.len(), delta.removed_records.len())
}
