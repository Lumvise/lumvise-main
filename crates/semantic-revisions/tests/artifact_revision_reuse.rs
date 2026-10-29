//! Contextual attachment inheritance must not manufacture a new immutable artifact body.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn published_artifact_with_new_owner() -> (ProjectFixture, SemanticArtifactRevisionWrite, String) {
    let mut project = ProjectFixture::new();
    project.initial();
    let original = SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "Unchanged note"),
        media_type: "text/plain".into(),
    };
    let mut save = project.edit("save-note", vec![]);
    save.structure = None;
    save.artifacts.push(original.clone());
    project.publish(&save);
    let advance = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let newer = accepted_id(&project.publish(&advance), "a");
    (project, original, newer)
}

#[test]
fn resaving_an_inherited_artifact_reuses_its_body_with_either_owner_revision() {
    for (use_current_owner, collect_owner) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let (mut project, original, newer) = published_artifact_with_new_owner();
        let before = project
            .state
            .resolve(&SemanticRecordRef::artifact("note"))
            .unwrap()
            .body;
        if collect_owner {
            project.collect();
            assert!(matches!(
                project
                    .state
                    .resolve(&SemanticRecordRef::element("a"))
                    .unwrap()
                    .body,
                SemanticRevisionBody::Pruned
            ));
        }
        let mut resave = project.edit("resave-note", vec![]);
        resave.structure = None;
        resave.artifacts.push(original.clone());
        if use_current_owner {
            resave.artifacts[0].artifact.semantic_element_id = newer.clone();
        }
        let receipt = project.publish(&resave);
        assert_eq!(accepted_id(&receipt, "note"), "note");
        assert!(receipt.revision_links.is_empty());
        assert_eq!(
            project
                .state
                .resolve(&SemanticRecordRef::artifact("note"))
                .unwrap()
                .body,
            before
        );
        assert!(
            project
                .state
                .selected_associations("working")
                .iter()
                .any(|association| association.artifact_id == "note"
                    && association.semantic_element_id == newer)
        );
    }
}

#[test]
fn changed_content_or_an_unrelated_owner_still_allocates_an_immutable_revision() {
    for unrelated_owner in [false, true] {
        let (mut project, mut write, current_owner) = published_artifact_with_new_owner();
        let before = project
            .state
            .resolve(&SemanticRecordRef::artifact("note"))
            .unwrap()
            .body;
        let expected_owner = if unrelated_owner {
            project
                .state
                .associations
                .push(SemanticRevisionAssociation {
                    association_id: "other-context-owner".into(),
                    artifact_id: "note".into(),
                    semantic_element_id: "b".into(),
                    source_context: "git:another-branch".into(),
                    created_at_ms: 3000,
                    inherited: false,
                });
            write.artifact.semantic_element_id = "b".into();
            "b".to_owned()
        } else {
            write.artifact.content = Some("Changed content".into());
            current_owner
        };
        let mut edit = project.edit("edit-note", vec![]);
        edit.structure = None;
        edit.artifacts.push(write);
        let receipt = project.publish(&edit);
        let accepted = accepted_id(&receipt, "note");
        assert_ne!(accepted, "note");
        assert_eq!(
            project
                .state
                .resolve(&SemanticRecordRef::artifact("note"))
                .unwrap()
                .body,
            before
        );
        let SemanticRevisionBody::Artifact(changed) = project
            .state
            .resolve(&SemanticRecordRef::artifact(&accepted))
            .unwrap()
            .body
        else {
            panic!("expected the accepted artifact body");
        };
        assert_eq!(changed.semantic_element_id, expected_owner);
    }
}

#[test]
fn combined_source_and_note_edits_attach_to_the_newly_accepted_owner() {
    for use_current_owner in [false, true] {
        let mut project = ProjectFixture::new();
        project.initial();
        let mut current_owner = "a".to_owned();
        for version in 2..5 {
            let mut request = project.edit(
                &format!("combined-{version}"),
                vec![
                    element("a", "a.rs", &format!("A{version}")),
                    element("b", "b.rs", "B1"),
                ],
            );
            let owner = if use_current_owner {
                current_owner.as_str()
            } else {
                "a"
            };
            request.artifacts.push(SemanticArtifactRevisionWrite {
                checkpoint: false,
                artifact: artifact("note", owner, &format!("N{version}")),
                media_type: "text/plain".into(),
            });
            let receipt = project.publish(&request);
            current_owner = accepted_id(&receipt, "a");
            let note = accepted_id(&receipt, "note");
            assert!(
                project
                    .state
                    .selected_associations("working")
                    .iter()
                    .any(|association| association.artifact_id == note
                        && association.semantic_element_id == current_owner),
                "note {note} must attach to accepted owner {current_owner} at version {version}"
            );
            let SemanticRevisionBody::Artifact(body) = project
                .state
                .resolve(&SemanticRecordRef::artifact(note))
                .unwrap()
                .body
            else {
                panic!("expected an accepted artifact body");
            };
            assert_eq!(body.semantic_element_id, current_owner);
        }
    }
}
