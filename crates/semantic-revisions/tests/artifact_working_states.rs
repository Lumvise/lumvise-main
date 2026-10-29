//! Saved edits coalesce until source history, a pin or a citation needs the exact state.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn note_edit(
    project: &ProjectFixture,
    operation: &str,
    id: &str,
    owner: &str,
    body: &str,
) -> SemanticRevisionPublication {
    let mut edit = project.edit(operation, vec![]);
    edit.structure = None;
    edit.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact(id, owner, body),
        media_type: "text/plain".into(),
    });
    edit
}

fn project_with_note() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    project.initial();
    let create = note_edit(&project, "create-note", "note", "a", "First");
    project.publish(&create);
    project
}

fn note_state(project: &ProjectFixture, id: &str) -> SemanticRevisionResolution {
    project
        .state
        .resolve(&SemanticRecordRef::artifact(id))
        .unwrap()
}

#[test]
fn repeated_saves_update_one_working_body_without_historical_links() {
    let mut project = project_with_note();
    for version in 2..5 {
        let edit = note_edit(
            &project,
            &format!("save-{version}"),
            "note",
            "a",
            &format!("Edit {version}"),
        );
        let receipt = project.publish(&edit);
        assert_eq!(accepted_id(&receipt, "note"), "note");
        assert!(receipt.revision_links.is_empty());
        assert_eq!(receipt.accepted_scope.artifact_edits["note"], version);
    }
    assert_eq!(
        project
            .state
            .records
            .iter()
            .filter(|row| row.record.kind == SemanticRecordKind::Artifact)
            .count(),
        1
    );
    assert_eq!(
        note_state(&project, "note").artifact_state,
        Some(SemanticArtifactRevisionState {
            edit_version: 4,
            frozen: false
        })
    );
}

#[test]
fn same_id_stale_writer_conflicts_and_replay_does_not_restore_old_working_content() {
    let mut project = project_with_note();
    let first = note_edit(&project, "first-writer", "note", "a", "First writer");
    let stale = note_edit(&project, "stale-writer", "note", "a", "Stale writer");
    let receipt = project.publish(&first);
    assert!(matches!(
        project.plan(&stale).outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    let latest = note_edit(&project, "latest-writer", "note", "a", "Latest");
    project.publish(&latest);
    let before = note_state(&project, "note");
    assert_eq!(project.publish(&first), receipt);
    assert_eq!(note_state(&project, "note"), before);
}

#[test]
fn advancing_source_freezes_old_state_and_later_saves_coalesce_in_its_successor() {
    let mut project = project_with_note();
    let edit = note_edit(&project, "last-old-edit", "note", "a", "Last state at A1");
    project.publish(&edit);
    let before = note_state(&project, "note").body;
    let source = project.edit(
        "advance-source",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let owner = accepted_id(&project.publish(&source), "a");
    assert!(note_state(&project, "note").artifact_state.unwrap().frozen);
    let edit = note_edit(&project, "first-new-edit", "note", &owner, "At A2");
    let successor = accepted_id(&project.publish(&edit), "note");
    assert_ne!(successor, "note");
    assert_eq!(note_state(&project, "note").body, before);
    let next = note_edit(
        &project,
        "second-new-edit",
        &successor,
        &owner,
        "Edited at A2",
    );
    assert_eq!(accepted_id(&project.publish(&next), &successor), successor);
    assert_eq!(
        note_state(&project, &successor)
            .artifact_state
            .unwrap()
            .edit_version,
        2
    );
}

#[test]
fn explicit_pin_freezes_working_body_even_after_the_pin_is_released() {
    let mut project = project_with_note();
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: project.selected().expectation.artifact_edits,
        operation_id: "pin-note".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "public".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::artifact("note")],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(&pin, &timestamp(5000), project.state.commit_version + 1)
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    let mut release = pin.clone();
    release.operation_id = "release-note".into();
    release.expected_generation = 1;
    release.records.clear();
    let plan = project
        .state
        .plan_selection(&release, &timestamp(6000), project.state.commit_version + 1)
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    let edit = note_edit(&project, "after-pin", "note", "a", "Changed after pin");
    assert_ne!(accepted_id(&project.publish(&edit), "note"), "note");
}

#[test]
fn a_citation_in_the_same_publication_freezes_the_original_working_body() {
    let mut project = project_with_note();
    let before = note_state(&project, "note").body;
    let mut edit = note_edit(&project, "cite-and-edit", "note", "a", "Next working state");
    let mut citation = artifact("citation", "b", "Evidence");
    citation.dependencies = serde_json::from_value(
        serde_json::json!([{"target":{"target_kind":"artifact","artifact_id":"note"}}]),
    )
    .unwrap();
    edit.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: citation,
        media_type: "text/plain".into(),
    });
    assert_ne!(accepted_id(&project.publish(&edit), "note"), "note");
    assert_eq!(note_state(&project, "note").body, before);
    assert!(note_state(&project, "note").artifact_state.unwrap().frozen);
}

#[test]
fn unrelated_artifact_edit_does_not_conflict_when_the_shared_generation_advances() {
    let mut project = project_with_note();
    let other = note_edit(&project, "create-other", "other", "b", "Other");
    project.publish(&other);
    let a = note_edit(&project, "edit-a", "note", "a", "A");
    let b = note_edit(&project, "edit-b", "other", "b", "B");
    project.publish(&a);
    project.publish(&b);
    assert_eq!(
        note_state(&project, "note")
            .artifact_state
            .unwrap()
            .edit_version,
        2
    );
    assert_eq!(
        note_state(&project, "other")
            .artifact_state
            .unwrap()
            .edit_version,
        2
    );
}

#[test]
fn reviewed_checkpoint_freezes_accepted_bytes_before_a_later_working_edit() {
    let mut project = project_with_note();
    let mut checkpoint = note_edit(
        &project,
        "reviewed-snapshot",
        "note",
        "a",
        "Reviewed source snapshot",
    );
    checkpoint.artifacts[0].checkpoint = true;
    let saved = accepted_id(&project.publish(&checkpoint), "note");
    assert_ne!(saved, "note");
    let frozen = note_state(&project, &saved);
    assert!(frozen.artifact_state.as_ref().unwrap().frozen);
    let next = note_edit(&project, "human-edit", &saved, "a", "Human interpretation");
    let working = accepted_id(&project.publish(&next), &saved);
    assert_ne!(working, saved);
    let history = note_state(&project, &saved);
    assert_eq!(history.body, frozen.body);
    assert_eq!(history.artifact_state, frozen.artifact_state);
    assert_eq!(history.successors[0].newer.record_id, working);
    let again = note_edit(
        &project,
        "human-edit-again",
        &working,
        "a",
        "Further interpretation",
    );
    assert_eq!(accepted_id(&project.publish(&again), &working), working);
}

#[test]
fn a_stale_working_token_cannot_pin_a_later_state() {
    let mut project = project_with_note();
    let original = project.selected().expectation.artifact_edits;
    let edit = note_edit(&project, "another-save", "note", "a", "New body");
    project.publish(&edit);
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: original,
        operation_id: "stale-pin".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "public".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::artifact("note")],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(&pin, &timestamp(5000), project.state.commit_version + 1)
        .unwrap();
    assert!(matches!(
        plan.outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    assert!(!note_state(&project, "note").artifact_state.unwrap().frozen);
}
