//! Public native persistence checks for coalesced working edits and exact frozen history.
use super::*;

fn exact_batch(client: &dyn SemanticPersistence, root: &str) -> Vec<SemanticElement> {
    let SemanticResult::Elements(elements) = apply(
        client,
        SemanticOperation::ElementsByIdsIncludingInactive {
            project_root: root.into(),
            semantic_element_ids: std::collections::HashSet::from(["a".into(), "missing".into()]),
        },
    ) else {
        panic!("expected exact elements")
    };
    elements
}

#[test]
fn exact_batches_preserve_historical_owners_respect_scope_and_never_substitute_pruned_ids() {
    let local = LocalPersistence::in_memory().unwrap();
    let original = element("a", "a.rs", 1);
    publish(&local, edit(&local, "initial", vec![original.clone()]));
    for revision in 2..=4 {
        publish(
            &local,
            edit(
                &local,
                &format!("source-{revision}"),
                vec![element("a", "a.rs", revision)],
            ),
        );
    }
    assert_eq!(exact_batch(&local, ROOT), vec![original]);
    assert!(exact_batch(&local, "/another-project").is_empty());
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) = apply(
        &local,
        SemanticOperation::PlanRevisionRetention {
            project_root: ROOT.into(),
            operation_id: "exact-read-collection".into(),
            policy: policy.clone(),
        },
    ) else {
        panic!("expected collection plan")
    };
    assert!(
        plan.records.contains(&SemanticRecordRef::element("a")),
        "{plan:?}"
    );
    apply(
        &local,
        SemanticOperation::CollectRevisionRetention { plan, policy },
    );
    assert!(exact_batch(&local, ROOT).is_empty());
    assert_ne!(
        selection(&local).unwrap().elements[0].semantic_element_id,
        "a"
    );
}

fn note_edit(
    client: &dyn SemanticPersistence,
    operation: &str,
    body: &str,
) -> SemanticRevisionPublication {
    let mut request = edit(client, operation, vec![]);
    request.structure = None;
    request.artifacts.push(evidence_artifact(
        "note",
        body,
        ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    ));
    request
}

pub(super) fn pin_artifact(client: &dyn SemanticPersistence, id: &str) {
    apply(
        client,
        SemanticOperation::SelectRevisions(SemanticRevisionSelectionUpdate {
            artifact_edits: selection(client).unwrap().expectation.artifact_edits,
            operation_id: format!("pin-{id}"),
            project_root: ROOT.into(),
            project_id: project_lineage_id(client),
            selection_id: format!("pin:{id}"),
            source_context: "git:main".into(),
            expected_generation: 0,
            mode: SemanticSelectionMode::Pinned,
            records: vec![SemanticRecordRef::artifact(id)],
            connection_ids: vec![],
            source_anchor: None,
        }),
    );
}

fn resolved(client: &dyn SemanticPersistence, id: &str) -> SemanticRevisionResolution {
    match apply(
        client,
        SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record: SemanticRecordRef::artifact(id),
        },
    ) {
        SemanticResult::RevisionResolution(Some(resolution)) => resolution,
        other => panic!("{other:?}; expected exact artifact resolution"),
    }
}

#[test]
fn native_same_id_edits_replace_bytes_invalidate_vectors_and_reject_original_stale_tokens() {
    let local = LocalPersistence::in_memory().unwrap();
    save_revision_note(&local);
    store_note_vector(&local);
    let old_reference = selection(&local).unwrap().artifacts[0]
        .content_ref
        .clone()
        .unwrap();
    let stale = note_edit(&local, "stale", "Stale overwrite");
    let update = note_edit(&local, "current", "New working body");
    let receipt = publish(&local, update.clone());
    assert_eq!(receipt.remaps[0].accepted.record_id, "note");
    assert_eq!(receipt.accepted_scope.artifact_edits["note"], 2);
    assert!(receipt.revision_links.is_empty());
    assert_eq!(
        selection(&local).unwrap().artifacts[0].content.as_deref(),
        Some("New working body")
    );
    assert!(vector_matches(&local).is_empty());
    assert!(matches!(
        apply(
            &local,
            SemanticOperation::ArtifactBlobGet {
                content_ref: old_reference
            }
        ),
        SemanticResult::ArtifactBlob(None)
    ));
    assert!(matches!(
        apply(&local, SemanticOperation::PublishRevisions(stale)),
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(_))
    ));
    assert_eq!(publish(&local, update), receipt);
    assert_eq!(
        resolved(&local, "note").artifact_state,
        Some(SemanticArtifactRevisionState {
            edit_version: 2,
            frozen: false
        })
    );
}

#[test]
fn replacing_a_working_body_keeps_bytes_still_referenced_by_another_artifact() {
    let local = LocalPersistence::in_memory().unwrap();
    save_revision_note(&local);
    let old_reference = selection(&local).unwrap().artifacts[0]
        .content_ref
        .clone()
        .unwrap();
    let mut copy = note_edit(&local, "same-bytes-copy", "exact body");
    copy.artifacts[0].artifact.artifact_id = "copy".into();
    publish(&local, copy);
    publish(
        &local,
        note_edit(&local, "replace-note", "New note content"),
    );
    assert!(
        matches!(apply(&local, SemanticOperation::ArtifactBlobGet { content_ref: old_reference }), SemanticResult::ArtifactBlob(Some(blob)) if blob.content == b"exact body")
    );
    let selected = selection(&local).unwrap();
    assert!(
        selected
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == "copy"
                && artifact.content.as_deref() == Some("exact body"))
    );
}

#[test]
fn native_source_advance_preserves_the_last_working_bytes_and_edits_the_successor_in_place() {
    let local = LocalPersistence::in_memory().unwrap();
    save_revision_note(&local);
    publish(&local, note_edit(&local, "last-working", "Last at A1"));
    publish(
        &local,
        edit(&local, "source-advance", vec![element("a", "a.rs", 2)]),
    );
    assert!(resolved(&local, "note").artifact_state.unwrap().frozen);
    let successor = publish(&local, note_edit(&local, "new-source-note", "First at A2")).remaps[0]
        .accepted
        .record_id
        .clone();
    assert_ne!(successor, "note");
    let receipt = publish(
        &local,
        note_edit(&local, "new-source-second-save", "Second at A2"),
    );
    assert_eq!(receipt.remaps[0].accepted.record_id, successor);
    let SemanticRevisionBody::Artifact(old) = resolved(&local, "note").body else {
        panic!("expected frozen old body");
    };
    assert_eq!(old.content.as_deref(), Some("Last at A1"));
    assert_eq!(
        selection(&local).unwrap().artifacts[0].content.as_deref(),
        Some("Second at A2")
    );
}

#[test]
fn reopening_a_database_preserves_working_tokens_and_latest_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("working.db");
    {
        let local = LocalPersistence::open(path.to_str().unwrap()).unwrap();
        save_revision_note(&local);
        publish(
            &local,
            note_edit(&local, "before-close", "Saved before close"),
        );
    }
    let local = LocalPersistence::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        resolved(&local, "note")
            .artifact_state
            .unwrap()
            .edit_version,
        2
    );
    let receipt = publish(
        &local,
        note_edit(&local, "after-reopen", "Saved after reopen"),
    );
    assert_eq!(receipt.accepted_scope.artifact_edits["note"], 3);
    assert_eq!(selection(&local).unwrap().artifacts[0].artifact_id, "note");
    assert_eq!(
        selection(&local).unwrap().artifacts[0].content.as_deref(),
        Some("Saved after reopen")
    );
}
