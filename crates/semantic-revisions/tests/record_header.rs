mod common;
use common::*;
use lumvise_semantic_revisions::*;

#[test]
fn portable_header_restores_exact_identity_and_canonical_lifecycle() {
    let mut project = ProjectFixture::new();
    project.initial();
    let stored = project.state.records[0].clone();
    let header = SemanticRevisionRecordHeader::from(&stored);
    let mut projection = stored.body.clone();
    if let SemanticRevisionBody::Element(element) = &mut projection {
        element.lifecycle = "inactive".into();
    }
    assert_eq!(header.clone().restore(projection).unwrap(), stored);
    let mut wrong = stored.body.clone();
    if let SemanticRevisionBody::Element(element) = &mut wrong {
        element.semantic_element_id = "different-revision".into();
    }
    assert!(header.clone().restore(wrong).is_err());
    assert!(header.restore(SemanticRevisionBody::Pruned).is_err());
}

#[test]
fn portable_tombstone_cannot_resurrect_a_payload() {
    let mut project = ProjectFixture::new();
    project.initial();
    let stored = project.state.records[0].clone();
    let mut header = SemanticRevisionRecordHeader::from(stored.clone());
    header.pruned_at_ms = Some(10_000);
    assert!(header.clone().restore(stored.body).is_err());
    let pruned = header.restore(SemanticRevisionBody::Pruned).unwrap();
    assert_eq!(pruned.record, stored.record);
    assert_eq!(pruned.pruned_at_ms, Some(10_000));
}

#[test]
fn an_unavailable_header_cannot_accept_a_substitute_payload() {
    let mut project = ProjectFixture::new();
    project.initial();
    let stored = project.state.records[0].clone();
    let mut encoded = serde_json::to_value(SemanticRevisionRecordHeader::from(&stored)).unwrap();
    encoded["unavailable"] = serde_json::json!({
        "recovery_operation_id": "acknowledge-missing-body",
        "unavailable_at_ms": 10_000,
        "canonical_lifecycle": "active"
    });
    let header: SemanticRevisionRecordHeader = serde_json::from_value(encoded).unwrap();
    assert!(
        header.restore(stored.body).is_err(),
        "an unavailable body must never be reconstructed from a caller's substitute"
    );
}
