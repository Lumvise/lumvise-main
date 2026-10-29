//! Exact payload dependencies belong to a revision, including when that revision is only evidence.
mod common;
use common::*;
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_semantic_revisions::*;

fn element_with_payload(id: &str, body: &str, payload: &str) -> SemanticElement {
    let mut encoded = serde_json::to_value(element(id, "@documents/note", body)).unwrap();
    encoded["revision_dependencies"] = serde_json::json!([SemanticRecordRef::artifact(payload)]);
    serde_json::from_value(encoded).unwrap()
}

fn write_payload(id: &str) -> SemanticArtifactRevisionWrite {
    SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact(id, "root", id),
        media_type: "text/plain".into(),
    }
}

fn initial_document() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit(
        "initial-document",
        vec![
            element("root", "@documents/root", "root"),
            element_with_payload("document", "first", "payload-1"),
        ],
    );
    initial.artifacts = vec![write_payload("payload-1")];
    project.publish(&initial);
    project
}

#[test]
fn selected_elements_keep_exact_payloads_outside_primary_membership() {
    let mut project = initial_document();
    let mut detach = project.edit("payload-not-primary", vec![]);
    detach.structure = None;
    detach.removed_artifact_ids = vec!["payload-1".into()];
    project.publish(&detach);
    let snapshot = project.selected();
    assert!(snapshot.artifacts.is_empty());
    assert!(
        snapshot
            .required_records
            .iter()
            .any(|stored| stored.record == SemanticRecordRef::artifact("payload-1"))
    );
    let receipt = project.collect();
    assert!(
        !receipt
            .pruned_records
            .contains(&SemanticRecordRef::artifact("payload-1"))
    );
}

#[test]
fn an_artifact_citing_an_older_element_keeps_that_elements_exact_payload() {
    let mut project = initial_document();
    let mut proof = artifact("proof", "root", "Old document evidence");
    proof.dependencies = vec![ArtifactDependency {
        target: ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "document".into(),
        },
    }];
    let mut update = project.edit(
        "next-document",
        vec![
            element("root", "@documents/root", "root"),
            element_with_payload("document", "second", "payload-2"),
        ],
    );
    update.artifacts = vec![
        write_payload("payload-2"),
        SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact: proof,
            media_type: "text/plain".into(),
        },
    ];
    update.removed_artifact_ids = vec!["payload-1".into()];
    project.publish(&update);
    let required = project
        .selected()
        .required_records
        .into_iter()
        .map(|stored| stored.record)
        .collect::<Vec<_>>();
    assert!(required.contains(&SemanticRecordRef::element("document")));
    assert!(required.contains(&SemanticRecordRef::artifact("payload-1")));
    let protected = project
        .state
        .plan_retention(
            "protected",
            &SemanticRevisionRetentionPolicy::Age {
                history_age_seconds: 1,
            },
            100_000,
        )
        .unwrap();
    assert!(
        !protected
            .records
            .contains(&SemanticRecordRef::artifact("payload-1"))
    );
    let mut release = project.edit("release-proof", vec![]);
    release.structure = None;
    release.removed_artifact_ids = vec!["proof".into()];
    project.publish(&release);
    let collected = project.collect();
    assert!(
        collected
            .pruned_records
            .contains(&SemanticRecordRef::element("document"))
    );
    assert!(
        collected
            .pruned_records
            .contains(&SemanticRecordRef::artifact("payload-1"))
    );
}

#[test]
fn publication_refuses_an_element_with_an_unavailable_exact_payload() {
    let project = ProjectFixture::new();
    let request = project.edit(
        "missing-payload",
        vec![element_with_payload("document", "body", "missing")],
    );
    let result = project
        .state
        .plan_publication(&request, &timestamp(1000), 1);
    assert!(
        result.is_err(),
        "expected missing exact payload to prevent publication"
    );
}

#[test]
fn element_dependency_validation_rejects_empty_and_duplicate_references() {
    for references in [
        vec![SemanticRecordRef::artifact("")],
        vec![
            SemanticRecordRef::artifact("payload"),
            SemanticRecordRef::artifact("payload"),
        ],
    ] {
        let mut encoded = serde_json::to_value(element("document", "doc", "body")).unwrap();
        encoded["revision_dependencies"] = serde_json::json!(references);
        let record: SemanticElement = serde_json::from_value(encoded).unwrap();
        assert!(
            record.validate().is_err(),
            "expected invalid exact dependencies {references:?} to fail"
        );
    }
}

#[test]
fn intrinsic_dependencies_include_the_parent_and_exact_payload_without_advancing_it() {
    let mut record = element_with_payload("document", "body", "payload-1");
    record.parent_element_id = Some("root".into());
    assert_eq!(
        record.required_revisions().collect::<Vec<_>>(),
        vec![
            SemanticRecordRef::element("root"),
            SemanticRecordRef::artifact("payload-1"),
        ]
    );
}
