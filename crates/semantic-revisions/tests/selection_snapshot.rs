//! Selected reads keep primary membership, contextual attachments and exact evidence distinct.
mod common;
use common::*;
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_semantic_revisions::*;

#[test]
fn selected_connections_serve_their_exact_older_endpoints_after_collection() {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit(
        "initial",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    initial
        .structure
        .as_mut()
        .unwrap()
        .relationships
        .push(SemanticRelationship {
            project_root: ROOT.into(),
            source_element_id: "a".into(),
            target_element_id: "b".into(),
            relationship_kind: "semantic".into(),
            label: "calls".into(),
            metadata: serde_json::json!({}),
        });
    project.publish(&initial);
    let mut update = project.edit("advance-b", vec![element("b", "b.rs", "B2")]);
    update.structure.as_mut().unwrap().partition = Some(SemanticPartition {
        project_root: ROOT.into(),
        provider_instance_id: None,
        replace_paths: vec!["b.rs".into()],
    });
    update.expected.records = vec![SemanticRecordRef::element("b")];
    update.expected.connection_ids.clear();
    project.publish(&update);
    assert!(
        !project
            .collect()
            .pruned_records
            .contains(&SemanticRecordRef::element("b"))
    );
    let selected = project.selected();
    assert!(
        !selected
            .expectation
            .records
            .contains(&SemanticRecordRef::element("b"))
    );
    assert!(
        selected
            .required_records
            .iter()
            .any(|record| record.record == SemanticRecordRef::element("b")
                && record.body == SemanticRevisionBody::Element(element("b", "b.rs", "B1")))
    );
}

fn save_note(project: &mut ProjectFixture, evidence: bool) {
    let mut note = artifact("note", "a", "Exact evidence");
    if evidence {
        note.dependencies.push(ArtifactDependency {
            target: ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: "a".into(),
            },
        });
    }
    let mut request = project.edit(
        "note",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: note,
        media_type: "text/plain".into(),
    });
    project.publish(&request);
}

fn advance_source(project: &mut ProjectFixture) -> String {
    let request = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    accepted_id(&project.publish(&request), "a")
}

#[test]
fn selected_reads_keep_exact_old_evidence_outside_current_membership() {
    let mut project = ProjectFixture::new();
    project.initial();
    save_note(&mut project, true);
    let newer = advance_source(&mut project);
    project.collect();
    let selected = project.selected();
    assert!(
        !selected
            .expectation
            .records
            .contains(&SemanticRecordRef::element("a"))
    );
    assert_eq!(selected.required_records.len(), 1);
    assert_eq!(
        selected.required_records[0].body,
        SemanticRevisionBody::Element(element("a", "a.rs", "A1"))
    );
    assert_eq!(selected.associations.len(), 1);
    assert_eq!(selected.associations[0].semantic_element_id, newer);
    assert_eq!(selected.artifacts[0].semantic_element_id, "a");
}

#[test]
fn public_reads_require_explicit_membership_for_old_artifact_evidence() {
    let mut project = ProjectFixture::new();
    project.initial();
    save_note(&mut project, true);
    advance_source(&mut project);
    let mut public = project.state.selection("working").unwrap().clone();
    public.selection.selection_id = "public".into();
    project.state.selections.push(public);
    let error = project.state.snapshot("public", "time").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("explicitly published dependency membership")
    );
    assert!(project.state.snapshot("working", "time").is_ok());
    project
        .state
        .selections
        .last_mut()
        .unwrap()
        .records
        .push(SemanticRecordRef::element("a"));
    let public = project.state.snapshot("public", "time").unwrap().unwrap();
    assert!(
        public
            .expectation
            .records
            .contains(&SemanticRecordRef::element("a"))
    );
}

#[test]
fn inherited_attachment_reads_do_not_require_its_pruned_original_owner() {
    let mut project = ProjectFixture::new();
    project.initial();
    save_note(&mut project, false);
    let newer = advance_source(&mut project);
    assert!(
        project
            .collect()
            .pruned_records
            .contains(&SemanticRecordRef::element("a"))
    );
    let selected = project.selected();
    assert!(selected.required_records.is_empty());
    assert_eq!(selected.artifacts[0].artifact_id, "note");
    assert_eq!(selected.associations[0].semantic_element_id, newer);
}

#[test]
fn other_source_context_associations_do_not_expand_selected_evidence() {
    let mut project = ProjectFixture::new();
    project.initial();
    save_note(&mut project, false);
    project.state.records.push(SemanticStoredRevision {
        artifact_state: None,
        record: SemanticRecordRef::element("secret"),
        element_scope: None,
        body: SemanticRevisionBody::Element(element("secret", "secret.rs", "Private branch")),
        created_at_ms: 1000,
        unselected_at_ms: None,
        pruned_at_ms: None,
    });
    project
        .state
        .associations
        .push(SemanticRevisionAssociation {
            association_id: "other-context".into(),
            artifact_id: "note".into(),
            semantic_element_id: "secret".into(),
            source_context: "git:private".into(),
            created_at_ms: 1000,
            inherited: false,
        });
    let selected = project.selected();
    assert!(selected.required_records.is_empty());
    assert!(
        selected
            .associations
            .iter()
            .all(|row| row.source_context == "git:main")
    );
}

#[test]
fn required_artifact_cycles_are_returned_once_without_selecting_them() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit(
        "cycle",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    for (id, dependency) in [("left", "right"), ("right", "left")] {
        let mut note = artifact(id, "a", id);
        note.dependencies.push(ArtifactDependency {
            target: ArtifactDependencyTarget::Artifact {
                edit_version: None,
                artifact_id: dependency.into(),
            },
        });
        request.artifacts.push(SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact: note,
            media_type: "text/plain".into(),
        });
    }
    project.publish(&request);
    project.state.selections[0]
        .records
        .retain(|reference| reference != &SemanticRecordRef::artifact("right"));
    let selected = project.selected();
    assert_eq!(selected.artifacts.len(), 1);
    assert_eq!(selected.required_records.len(), 1);
    assert_eq!(
        selected.required_records[0].record,
        SemanticRecordRef::artifact("right")
    );
}

#[test]
fn selected_reads_reject_missing_exact_evidence_and_connection_definitions() {
    let mut project = ProjectFixture::new();
    project.initial();
    save_note(&mut project, true);
    advance_source(&mut project);
    project
        .state
        .records
        .retain(|row| row.record != SemanticRecordRef::element("a"));
    let error = project.state.snapshot("working", "time").unwrap_err();
    assert!(
        matches!(error, SemanticRevisionError::InvalidValue { value, expected }
        if value == "a" && expected == "an available exact revision body in this project")
    );
    let mut project = ProjectFixture::new();
    project.initial();
    project.state.selections[0]
        .connection_ids
        .push("missing-edge".into());
    let error = project.state.snapshot("working", "time").unwrap_err();
    assert!(error.to_string().contains("missing-edge"));
}
