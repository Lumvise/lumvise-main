//! Producers can prepare exact references without copying the shared revision ID algorithm.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

#[test]
fn a_planned_payload_identity_matches_the_atomic_element_and_artifact_publication() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut initial = project.edit("initial-payload", vec![]);
    initial.structure = None;
    initial.artifacts = vec![SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("payload", "a", "First body"),
        media_type: "text/plain".into(),
    }];
    project.publish(&initial);
    let mut next = project.edit(
        "changed-payload",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let predicted = SemanticRecordRef::artifact(
        semantic_revision_successor_id(
            &next.project_id,
            &next.source_context,
            &next.operation_id,
            &SemanticRecordRef::artifact("payload"),
        )
        .unwrap(),
    );
    next.structure.as_mut().unwrap().elements[0].revision_dependencies = vec![predicted.clone()];
    next.artifacts = vec![SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("payload", "a", "Second body"),
        media_type: "text/plain".into(),
    }];
    let receipt = project.publish(&next);
    assert!(receipt.remaps.iter().any(|mapping| mapping.incoming
        == SemanticRecordRef::artifact("payload")
        && mapping.accepted == predicted));
    assert_eq!(
        project
            .selected()
            .elements
            .iter()
            .find(|e| e.path == "a.rs")
            .unwrap()
            .revision_dependencies,
        [predicted]
    );
    assert_eq!(project.publish(&next), receipt);
}

#[test]
fn successor_identity_is_scoped_deterministic_and_rejects_empty_components() {
    let previous = SemanticRecordRef::artifact("body");
    let stable = semantic_revision_successor_id("lineage", "local", "edit", &previous).unwrap();
    assert_eq!(
        semantic_revision_successor_id("lineage", "local", "edit", &previous).unwrap(),
        stable
    );
    for (project, context, operation, record) in [
        ("other", "local", "edit", previous.clone()),
        ("lineage", "branch", "edit", previous.clone()),
        ("lineage", "local", "next", previous.clone()),
        (
            "lineage",
            "local",
            "edit",
            SemanticRecordRef::element("body"),
        ),
    ] {
        assert_ne!(
            semantic_revision_successor_id(project, context, operation, &record).unwrap(),
            stable
        );
    }
    for (project, context, operation, record) in [
        ("", "local", "edit", previous.clone()),
        ("lineage", " ", "edit", previous.clone()),
        ("lineage", "local", "", previous),
        ("lineage", "local", "edit", SemanticRecordRef::artifact("")),
    ] {
        assert!(semantic_revision_successor_id(project, context, operation, &record).is_err());
    }
}
