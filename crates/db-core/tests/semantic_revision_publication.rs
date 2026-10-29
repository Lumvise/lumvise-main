//! Revision behavior through LocalPersistence and independent CentralizedPersistence clients.
#[path = "semantic_revision_publication/citation_retention.rs"]
mod citation_retention;
mod common;
#[path = "semantic_revision_publication/large_receipt.rs"]
mod large_receipt;
#[path = "semantic_revision_publication/working_states.rs"]
mod working_states;
use common::{LocalPublicationServer, RevisionTestClient};
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::sync::Arc;

const ROOT: &str = "/cooperation";

fn element(id: &str, path: &str, revision: i64) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: path.into(),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({ "revision": revision }),
    }
}

fn apply(semantic: &dyn SemanticPersistence, operation: SemanticOperation) -> SemanticResult {
    semantic
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn project_lineage_id(semantic: &dyn SemanticPersistence) -> String {
    RevisionTestClient::new(semantic, ROOT, "git:main").project_id()
}

#[test]
fn rejected_publications_leave_no_inline_bytes_or_graph_commit() {
    for wrong_lineage in [false, true] {
        let local = LocalPersistence::in_memory().unwrap();
        publish(
            &local,
            edit(&local, "initial", vec![element("a", "a.rs", 1)]),
        );
        let mut stale = edit(&local, "stale", vec![element("a", "a.rs", 3)]);
        let note = evidence_artifact(
            "rejected-note",
            "rejected bytes",
            ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: "a".into(),
            },
        );
        let content_ref = note
            .stored_artifact(&stale.project_id)
            .unwrap()
            .content_ref
            .unwrap();
        stale.artifacts.push(note);
        if wrong_lineage {
            stale.project_id = "another-lineage".into();
        } else {
            publish(
                &local,
                edit(&local, "winner", vec![element("a", "a.rs", 2)]),
            );
        }
        let before = selection(&local);
        let result = apply(&local, SemanticOperation::PublishRevisions(stale));
        assert!(matches!(
            result,
            SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(_))
        ));
        assert_eq!(selection(&local), before);
        assert!(matches!(
            apply(&local, SemanticOperation::ArtifactBlobGet { content_ref }),
            SemanticResult::ArtifactBlob(None)
        ));
    }
}

fn selection(semantic: &dyn SemanticPersistence) -> Option<SemanticRevisionSelectionSnapshot> {
    match apply(
        semantic,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    ) {
        SemanticResult::RevisionSelection(snapshot) => snapshot,
        other => panic!("{other:?}; expected a consistent revision selection"),
    }
}

fn edit(
    semantic: &dyn SemanticPersistence,
    operation_id: &str,
    elements: Vec<SemanticElement>,
) -> SemanticRevisionPublication {
    RevisionTestClient::new(semantic, ROOT, "git:main").publication(operation_id, elements)
}

fn publish(
    semantic: &dyn SemanticPersistence,
    request: SemanticRevisionPublication,
) -> SemanticRevisionPublicationReceipt {
    match apply(semantic, SemanticOperation::PublishRevisions(request)) {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt,
        other => panic!("{other:?}; expected accepted immutable revisions"),
    }
}

fn path_edit(request: &mut SemanticRevisionPublication, path: &str) {
    request.structure.as_mut().unwrap().partition = Some(SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec![path.into()],
        provider_instance_id: None,
    });
}

fn evidence_artifact(
    id: &str,
    body: &str,
    target: ArtifactDependencyTarget,
) -> SemanticArtifactRevisionWrite {
    SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: SemanticArtifact {
            artifact_id: id.into(),
            semantic_element_id: "a".into(),
            artifact_kind: "annotation".into(),
            title: id.into(),
            content_ref: None,
            content: Some(body.into()),
            searchable_text: None,
            content_size_bytes: None,
            dependencies: vec![ArtifactDependency { target }],
            metadata: json!({}),
        },
        media_type: "text/plain".into(),
    }
}

fn revision_embedding() -> ArtifactTextVector {
    ArtifactTextVector {
        engine_id: "test".into(),
        model: None,
        dimensions: 2,
        vector: vec![1.0, 0.0],
        normalized: true,
        metadata: json!({}),
    }
}

fn store_note_vector(semantic: &dyn SemanticPersistence) {
    apply(
        semantic,
        SemanticOperation::StoreArtifactTextVectors {
            project_root: ROOT.into(),
            vectors: vec![StoredArtifactTextVector {
                artifact_id: "note".into(),
                semantic_element_id: "a".into(),
                source_text: "exact body".into(),
                vector: revision_embedding(),
            }],
        },
    );
}

fn vector_matches(semantic: &dyn SemanticPersistence) -> Vec<VectorSearchResult> {
    match apply(
        semantic,
        SemanticOperation::SearchArtifactTextVectors {
            project_root: ROOT.into(),
            query: vec![1.0, 0.0],
            k: 10,
            engine_id: "test".into(),
            model: None,
        },
    ) {
        SemanticResult::ArtifactVectorSearch(found) => found,
        other => panic!("{other:?}; expected current artifact vector matches"),
    }
}

fn save_revision_note(semantic: &dyn SemanticPersistence) {
    publish(
        semantic,
        edit(semantic, "initial", vec![element("a", "a.rs", 1)]),
    );
    let mut notes = edit(semantic, "note", vec![element("a", "a.rs", 1)]);
    notes.artifacts.push(evidence_artifact(
        "note",
        "exact body",
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    ));
    publish(semantic, notes);
}

#[test]
fn revision_projection_vector_search_and_writes_follow_current_artifact_attachments() {
    let local = LocalPersistence::in_memory().unwrap();
    save_revision_note(&local);
    store_note_vector(&local);
    assert_eq!(vector_matches(&local)[0].id, "note");
    publish(
        &local,
        edit(&local, "advance", vec![element("a", "a.rs", 2)]),
    );
    assert_eq!(vector_matches(&local).len(), 1);
    store_note_vector(&local);
    let mut changed = edit(&local, "change-note", vec![element("a", "a.rs", 2)]);
    changed.artifacts.push(evidence_artifact(
        "note",
        "updated body",
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    ));
    publish(&local, changed);
    assert!(
        vector_matches(&local).is_empty(),
        "an older note's vector must leave the current view"
    );
}

#[test]
fn revision_projection_name_vectors_exclude_retired_source_revisions() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(
        &local,
        edit(&local, "initial", vec![element("a", "a.rs", 1)]),
    );
    apply(
        &local,
        SemanticOperation::StoreElementNameVectors {
            project_root: ROOT.into(),
            vectors: vec![StoredSemanticElementNameVector {
                semantic_element_id: "a".into(),
                project_root: ROOT.into(),
                source_text: "a".into(),
                vector: revision_embedding(),
            }],
        },
    );
    publish(
        &local,
        edit(&local, "advance", vec![element("a", "a.rs", 2)]),
    );
    let SemanticResult::ElementVectorSearch(found) = apply(
        &local,
        SemanticOperation::SearchElementNameVectors {
            project_root: ROOT.into(),
            query: vec![1.0, 0.0],
            k: 10,
            engine_id: "test".into(),
            model: None,
        },
    ) else {
        panic!("expected current source vector matches");
    };
    assert!(
        found.is_empty(),
        "retained source history must not appear in current search"
    );
}

fn assert_current_owner_changed(
    semantic: &dyn SemanticPersistence,
    owner: &str,
    before: i64,
    after: i64,
) {
    let SemanticResult::ChangesSinceRevision(batch) = apply(
        semantic,
        SemanticOperation::ChangesSinceRevision {
            scope: ChangeHookScope {
                project_root: ROOT.into(),
                entity_kinds: Default::default(),
            },
            after_revision: before,
            limit: 10,
        },
    ) else {
        panic!("expected a current owner change batch");
    };
    assert_eq!(
        batch.changed,
        vec![ChangedElement {
            element_id: owner.into(),
            entity_kind: "function".into(),
            revision: after,
            disposition: ChangeDisposition::Upserted
        }]
    );
}

#[test]
fn revision_projection_artifact_edits_redirty_current_owners_before_and_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cooperation.db");
    let (owner, before, after) = {
        let local = LocalPersistence::open(&path).unwrap();
        save_revision_note(&local);
        publish(
            &local,
            edit(&local, "advance", vec![element("a", "a.rs", 2)]),
        );
        let selected = selection(&local).unwrap();
        let owner = selected.elements[0].semantic_element_id.clone();
        let mut changed = edit(&local, "edit-note", vec![element("a", "a.rs", 2)]);
        changed.artifacts.push(evidence_artifact(
            "note",
            "updated body",
            ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: "a".into(),
            },
        ));
        let receipt = publish(&local, changed);
        assert_current_owner_changed(
            &local,
            &owner,
            selected.commit_version,
            receipt.commit_version,
        );
        (owner, selected.commit_version, receipt.commit_version)
    };
    let local = LocalPersistence::open(&path).unwrap();
    assert_current_owner_changed(&local, &owner, before, after);
}

#[test]
fn resource_selected_reads_return_exact_old_evidence_and_current_attachment_owners() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let client = CentralizedPersistence::new(Arc::new(LocalPublicationServer(local)), "checkout");
    publish(
        &client,
        edit(&client, "initial", vec![element("a", "a.rs", 1)]),
    );
    let mut notes = edit(&client, "notes", vec![element("a", "a.rs", 1)]);
    notes.artifacts.push(evidence_artifact(
        "note",
        "exact old body",
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    ));
    notes.artifacts.push(evidence_artifact(
        "report",
        "cites the old note",
        ArtifactDependencyTarget::Artifact {
            edit_version: None,
            artifact_id: "note".into(),
        },
    ));
    publish(&client, notes);
    let mut changed = edit(&client, "advance", vec![element("a", "a.rs", 2)]);
    changed.artifacts.push(evidence_artifact(
        "note",
        "new body",
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    ));
    publish(&client, changed);
    let selected = selection(&client).unwrap();
    let old = selected
        .required_records
        .iter()
        .find(|row| row.record == SemanticRecordRef::artifact("note"))
        .unwrap();
    let SemanticRevisionBody::Artifact(old) = &old.body else {
        panic!("expected an exact required artifact");
    };
    assert_eq!(old.content.as_deref(), Some("exact old body"));
    assert!(
        selected
            .required_records
            .iter()
            .any(|row| row.body == SemanticRevisionBody::Element(element("a", "a.rs", 1)))
    );
    assert!(
        !selected
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == "note")
    );
    assert_eq!(selected.associations.len(), selected.artifacts.len());
    assert!(
        selected
            .associations
            .iter()
            .all(|row| row.semantic_element_id == selected.elements[0].semantic_element_id)
    );
}

#[test]
fn archive_relocation_keeps_project_identity_replays_and_unchanged_inline_artifact_ids() {
    let source = LocalPersistence::in_memory().unwrap();
    publish(
        &source,
        edit(&source, "initial", vec![element("a", "a.rs", 1)]),
    );
    let mut note = edit(&source, "note", vec![element("a", "a.rs", 1)]);
    note.artifacts.push(evidence_artifact(
        "note",
        "exact body",
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    ));
    let original = publish(&source, note.clone());
    let SemanticResult::ProjectArchive(archive) = apply(
        &source,
        SemanticOperation::ProjectArchive {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected a captured archive");
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relocated.pz");
    archive
        .write(&path, &InvocationControl::sixty_seconds())
        .unwrap();
    let target = LocalPersistence::in_memory().unwrap();
    apply(
        &target,
        SemanticOperation::ImportPzSnapshot {
            project_root: "/relocated".into(),
            input_path: path.to_string_lossy().into_owned(),
        },
    );
    let SemanticResult::ProjectIdentity(identity) = apply(
        &target,
        SemanticOperation::ProjectIdentity {
            project_root: "/relocated".into(),
        },
    ) else {
        panic!("expected imported project identity");
    };
    assert_eq!(identity.project_id, archive.project_id);
    note.project_root = "/relocated".into();
    for element in &mut note.structure.as_mut().unwrap().elements {
        element.project_root = "/relocated".into();
    }
    let replay = publish(&target, note.clone());
    assert_eq!(replay.commit_version, original.commit_version);
    assert_eq!(replay.remaps, original.remaps);
    let SemanticResult::RevisionSelection(Some(selected)) = apply(
        &target,
        SemanticOperation::RevisionSelection {
            project_root: "/relocated".into(),
            selection_id: "working".into(),
        },
    ) else {
        panic!("expected relocated selection");
    };
    note.operation_id = "unchanged-at-new-root".into();
    note.expected = selected.expectation;
    note.structure = None;
    let receipt = publish(&target, note);
    assert_eq!(
        receipt.remaps[0].accepted,
        SemanticRecordRef::artifact("note")
    );
    assert!(receipt.revision_links.is_empty());
}

#[test]
fn resource_membership_reads_and_known_source_anchors_restore_the_exact_old_ids() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let client = CentralizedPersistence::new(Arc::new(LocalPublicationServer(local)), "checkout");
    let anchor = SemanticSourceAnchor {
        revision: "git-commit-1".into(),
        canonical_remote: Some("https://example.invalid/repo.git".into()),
        dirty: false,
    };
    let mut initial = edit(
        &client,
        "initial",
        vec![element("a", "a.rs", 1), element("b", "b.rs", 1)],
    );
    initial.source_anchor = Some(anchor.clone());
    publish(&client, initial);
    let original = selection(&client).unwrap().expectation;
    let SemanticResult::RevisionObservation(Some(scoped)) = apply(
        &client,
        SemanticOperation::RevisionSelectionMembers(SemanticRevisionSelectionQuery {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            artifact_ids: vec![],
            structure_coverage: SemanticRevisionStructureCoverage::Partition(SemanticPartition {
                project_root: ROOT.into(),
                replace_paths: vec!["a.rs".into()],
                provider_instance_id: None,
            }),
        }),
    ) else {
        panic!("expected scoped authoritative membership");
    };
    assert_eq!(
        scoped.expected.records,
        vec![SemanticRecordRef::element("a")]
    );
    let mut update = edit(
        &client,
        "source-2",
        vec![element("a", "a.rs", 2), element("b", "b.rs", 1)],
    );
    update.source_anchor = Some(SemanticSourceAnchor {
        revision: "git-commit-2".into(),
        ..anchor.clone()
    });
    publish(&client, update);
    let SemanticResult::RevisionMembership(Some(known)) = apply(
        &client,
        SemanticOperation::RevisionSourceAnchor {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            anchor: anchor.clone(),
        },
    ) else {
        panic!("expected previously indexed source anchor");
    };
    assert_eq!(known.expectation(&[]), original);
    let SemanticResult::RevisionMembership(Some(historical)) = apply(
        &client,
        SemanticOperation::RevisionSelectionAt {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            generation: 1,
        },
    ) else {
        panic!("expected historical membership");
    };
    assert_eq!(historical, known);
    let generation = selection(&client).unwrap().selection.generation;
    let restored = apply(
        &client,
        SemanticOperation::SelectRevisions(SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: "select-known-checkout".into(),
            project_root: ROOT.into(),
            project_id: project_lineage_id(&client),
            selection_id: "working".into(),
            source_context: "git:main".into(),
            expected_generation: generation,
            mode: SemanticSelectionMode::FollowAccepted,
            records: known.records,
            connection_ids: known.connection_ids,
            source_anchor: Some(anchor),
        }),
    );
    assert!(matches!(
        restored,
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
    ));
    assert_eq!(
        selection(&client).unwrap().elements,
        vec![element("a", "a.rs", 1), element("b", "b.rs", 1)]
    );
}

#[test]
fn a_second_empty_writable_selection_cannot_claim_an_existing_source_context() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(
        &local,
        edit(&local, "initial", vec![element("a", "a.rs", 1)]),
    );
    let mut duplicate = edit(&local, "duplicate-context", vec![]);
    duplicate.selection_id = "other-writer-head".into();
    duplicate.expected = SemanticSelectionExpectation::default();
    path_edit(&mut duplicate, "empty.rs");
    let error = SemanticPersistence::execute(
        &local,
        SemanticOperation::PublishRevisions(duplicate),
        &InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("shared follow-accepted selection `working`")
    );
}

#[test]
fn independent_clients_keep_disjoint_work_and_receive_typed_overlap_conflicts() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let first = CentralizedPersistence::new(
        Arc::new(LocalPublicationServer(local.clone())),
        "first-instance",
    );
    let second =
        CentralizedPersistence::new(Arc::new(LocalPublicationServer(local)), "second-instance");
    publish(
        &first,
        edit(
            &first,
            "initial",
            vec![element("a", "a.rs", 1), element("b", "b.rs", 1)],
        ),
    );
    let mut a = edit(&first, "a2", vec![element("a", "a.rs", 2)]);
    path_edit(&mut a, "a.rs");
    let mut b = edit(&second, "b2", vec![element("b", "b.rs", 2)]);
    path_edit(&mut b, "b.rs");
    let mut stale = a.clone();
    stale.operation_id = "stale-a".into();
    let a = std::thread::spawn(move || publish(&first, a));
    let b = publish(&second, b);
    let a = a.join().unwrap();
    assert_ne!(a.remaps[0].accepted.record_id, "a");
    assert_ne!(b.remaps[0].accepted.record_id, "b");
    assert!(
        selection(&second)
            .unwrap()
            .elements
            .iter()
            .all(|record| record.metadata["revision"] == 2)
    );
    assert!(matches!(
        apply(&second, SemanticOperation::PublishRevisions(stale)),
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(_))
    ));
}

#[test]
fn exact_body_and_lost_ack_receipt_survive_a_real_database_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("revisions.db");
    let request;
    let receipt;
    {
        let local = LocalPersistence::open(&path).unwrap();
        publish(
            &local,
            edit(&local, "initial", vec![element("a", "a.rs", 1)]),
        );
        request = edit(&local, "lost-ack", vec![element("a", "a.rs", 2)]);
        receipt = publish(&local, request.clone());
        let SemanticResult::Element(Some(old)) = apply(
            &local,
            SemanticOperation::Element {
                semantic_element_id: "a".into(),
            },
        ) else {
            panic!("expected exact old element");
        };
        assert_eq!(old, element("a", "a.rs", 1));
        let SemanticResult::ProjectSnapshot(current) = apply(
            &local,
            SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot(ROOT.into()),
                artifact_namespace: None,
            },
        ) else {
            panic!("expected current project view");
        };
        assert_eq!(current.elements.len(), 1);
        assert_eq!(
            current.elements[0].semantic_element_id,
            receipt.remaps[0].accepted.record_id
        );
    }
    let local = LocalPersistence::open(&path).unwrap();
    assert_eq!(publish(&local, request), receipt);
    let result = apply(
        &local,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record: SemanticRecordRef::element("a"),
        },
    );
    let SemanticResult::RevisionResolution(Some(resolution)) = result else {
        panic!("expected exact historical resolution");
    };
    assert_eq!(
        resolution.body,
        SemanticRevisionBody::Element(element("a", "a.rs", 1))
    );
    assert_eq!(
        resolution.successors[0].newer.record_id,
        receipt.remaps[0].accepted.record_id
    );
}

#[test]
fn an_accepted_operation_cannot_be_reused_for_different_content() {
    let local = LocalPersistence::in_memory().unwrap();
    let request = edit(&local, "one-operation", vec![element("a", "a.rs", 1)]);
    publish(&local, request.clone());
    let mut changed = request;
    changed.structure.as_mut().unwrap().elements[0].name = "different".into();
    let error = SemanticPersistence::execute(
        &local,
        SemanticOperation::PublishRevisions(changed),
        &InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("identical content"));
    assert_eq!(selection(&local).unwrap().elements[0].name, "a");
}

#[test]
fn a_current_selection_is_protected_and_a_new_pin_invalidates_an_earlier_sweep() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(
        &local,
        edit(&local, "initial", vec![element("a", "a.rs", 1)]),
    );
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let result = apply(
        &local,
        SemanticOperation::PlanRevisionRetention {
            project_root: ROOT.into(),
            operation_id: "sweep".into(),
            policy: policy.clone(),
        },
    );
    let SemanticResult::RevisionRetentionPlan(plan) = result else {
        panic!("expected collection plan");
    };
    assert!(plan.records.is_empty());
    apply(
        &local,
        SemanticOperation::SelectRevisions(SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: "public-pin".into(),
            project_root: ROOT.into(),
            project_id: project_lineage_id(&local),
            selection_id: "public".into(),
            source_context: "git:main".into(),
            expected_generation: 0,
            mode: SemanticSelectionMode::Pinned,
            records: vec![SemanticRecordRef::element("a")],
            connection_ids: vec![],
            source_anchor: None,
        }),
    );
    assert!(
        SemanticPersistence::execute(
            &local,
            SemanticOperation::CollectRevisionRetention { plan, policy },
            &InvocationControl::sixty_seconds()
        )
        .is_err()
    );
    assert_eq!(
        selection(&local).unwrap().elements,
        vec![element("a", "a.rs", 1)]
    );
}

#[test]
fn artifact_bytes_remain_exact_across_revisions_and_replayed_writes() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(
        &local,
        edit(&local, "initial", vec![element("a", "a.rs", 1)]),
    );
    let mut request = edit(&local, "note", vec![element("a", "a.rs", 1)]);
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: SemanticArtifact {
            artifact_id: "note".into(),
            semantic_element_id: "a".into(),
            artifact_kind: "annotation".into(),
            title: "Note".into(),
            content_ref: None,
            content: Some("first body".into()),
            searchable_text: None,
            content_size_bytes: None,
            dependencies: vec![],
            metadata: json!({}),
        },
        media_type: "text/plain".into(),
    });
    publish(&local, request.clone());
    working_states::pin_artifact(&local, "note");
    let mut second = request;
    second.operation_id = "note-edit".into();
    second.expected = selection(&local).unwrap().expectation;
    second.artifacts[0].artifact.content = Some("second body".into());
    let receipt = publish(&local, second.clone());
    assert_eq!(publish(&local, second), receipt);
    let SemanticResult::RevisionResolution(Some(old)) = apply(
        &local,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record: SemanticRecordRef::artifact("note"),
        },
    ) else {
        panic!("expected artifact history");
    };
    let SemanticRevisionBody::Artifact(old) = old.body else {
        panic!("expected artifact body");
    };
    assert_eq!(old.content.as_deref(), Some("first body"));
    assert_eq!(
        selection(&local).unwrap().artifacts[0].content.as_deref(),
        Some("second body")
    );
}

#[test]
fn a_blob_reference_cannot_be_overwritten_with_different_bytes() {
    let local = LocalPersistence::in_memory().unwrap();
    let operation = SemanticOperation::ArtifactBlobPut {
        content_ref: "blob:immutable".into(),
        artifact_id: "note".into(),
        media_type: "text/plain".into(),
        content: b"first".to_vec(),
    };
    apply(&local, operation.clone());
    apply(&local, operation);
    let error = SemanticPersistence::execute(
        &local,
        SemanticOperation::ArtifactBlobPut {
            content_ref: "blob:immutable".into(),
            artifact_id: "note".into(),
            media_type: "text/plain".into(),
            content: b"changed".to_vec(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("immutable blob reference"));
    let SemanticResult::ArtifactBlob(Some(blob)) = apply(
        &local,
        SemanticOperation::ArtifactBlobGet {
            content_ref: "blob:immutable".into(),
        },
    ) else {
        panic!("expected original bytes");
    };
    assert_eq!(blob.content, b"first");
}

#[test]
fn shared_large_content_is_hydrated_for_every_selected_artifact() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(
        &local,
        edit(&local, "initial", vec![element("a", "a.rs", 1)]),
    );
    let text = "A shared long artifact body. ".repeat(500);
    let mut request = edit(&local, "shared-content", vec![element("a", "a.rs", 1)]);
    for id in ["first", "second"] {
        request.artifacts.push(SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact: SemanticArtifact {
                artifact_id: id.into(),
                semantic_element_id: "a".into(),
                artifact_kind: "annotation".into(),
                title: id.into(),
                content_ref: None,
                content: Some(text.clone()),
                searchable_text: None,
                content_size_bytes: None,
                dependencies: vec![],
                metadata: json!({}),
            },
            media_type: "text/plain".into(),
        });
    }
    publish(&local, request);
    let selected = selection(&local).unwrap();
    assert_eq!(selected.artifacts.len(), 2);
    assert_eq!(
        selected.artifacts[0].content_ref,
        selected.artifacts[1].content_ref
    );
    assert!(
        selected
            .artifacts
            .iter()
            .all(|artifact| artifact.content.as_ref() == Some(&text))
    );
}

#[test]
fn unconditional_mutators_cannot_change_or_delete_revision_managed_bodies() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(
        &local,
        edit(&local, "initial", vec![element("a", "a.rs", 1)]),
    );
    for operation in [
        SemanticOperation::SyncStructure {
            project_root: ROOT.into(),
            elements: vec![element("a", "a.rs", 99)],
            relationships: vec![],
        },
        SemanticOperation::RemoveElement {
            semantic_element_id: "a".into(),
        },
        SemanticOperation::UpsertArtifact {
            artifact: SemanticArtifact {
                artifact_id: "unguarded".into(),
                semantic_element_id: "a".into(),
                artifact_kind: "annotation".into(),
                title: "Note".into(),
                content_ref: None,
                content: Some("Must not be published".into()),
                searchable_text: None,
                content_size_bytes: None,
                dependencies: vec![],
                metadata: json!({}),
            },
            media_type: "text/plain".into(),
        },
    ] {
        let error =
            SemanticPersistence::execute(&local, operation, &InvocationControl::sixty_seconds())
                .unwrap_err();
        assert!(error.to_string().contains("conditional"), "{error}");
    }
    assert_eq!(
        selection(&local).unwrap().elements,
        vec![element("a", "a.rs", 1)]
    );
    assert!(selection(&local).unwrap().artifacts.is_empty());
}
