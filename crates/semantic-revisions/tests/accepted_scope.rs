//! Receipts advance only the content a checkout published, including connection occurrences.
mod common;
use common::*;
use lumvise_semantic_revisions::*;
use serde_json::json;

fn call() -> SemanticRelationship {
    SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }
}

fn path_request(
    project: &ProjectFixture,
    id: &str,
    file: &str,
    body: &str,
) -> SemanticRevisionPublication {
    let record = file.strip_suffix(".rs").unwrap();
    let mut request = project.edit(id, vec![element(record, file, body)]);
    path_edit(&mut request, file);
    request
}

#[test]
fn scoped_acknowledgement_does_not_adopt_another_instances_untouched_content() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut a = path_request(&project, "client-a", "a.rs", "A2");
    a.structure.as_mut().unwrap().relationships = vec![call(), call()];
    let b = path_request(&project, "client-b", "b.rs", "B2");
    let b_receipt = project.publish(&b);
    let receipt = project.publish(&a);
    assert_eq!(
        receipt.accepted_scope.records,
        vec![SemanticRecordRef::element(accepted_id(&receipt, "a"))]
    );
    assert_eq!(receipt.accepted_scope.connection_ids.len(), 2);
    assert_ne!(
        receipt.accepted_scope.connection_ids[0],
        receipt.accepted_scope.connection_ids[1]
    );
    assert_eq!(
        receipt.accepted_scope.generation,
        receipt.selection.generation
    );
    assert!(
        project
            .selected()
            .expectation
            .records
            .contains(&SemanticRecordRef::element(accepted_id(&b_receipt, "b")))
    );
    let later = path_request(&project, "client-b-later", "b.rs", "B3");
    project.publish(&later);
    assert_eq!(project.publish(&a), receipt);
}

#[test]
fn removing_the_last_owned_body_acknowledges_an_empty_scope() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit("delete-a", vec![]);
    path_edit(&mut request, "a.rs");
    let receipt = project.publish(&request);
    assert!(receipt.accepted_scope.records.is_empty());
    assert!(receipt.accepted_scope.connection_ids.is_empty());
    assert_eq!(project.selected().elements.len(), 1);
}

#[test]
fn an_artifact_only_acknowledgement_leaves_source_membership_outside_its_scope() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit("note", vec![]);
    request.structure = None;
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "Note"),
        media_type: "text/plain".into(),
    });
    let receipt = project.publish(&request);
    assert_eq!(
        receipt.accepted_scope.records,
        vec![SemanticRecordRef::artifact("note")]
    );
    assert!(receipt.accepted_scope.connection_ids.is_empty());
    assert_eq!(project.selected().elements.len(), 2);
}
