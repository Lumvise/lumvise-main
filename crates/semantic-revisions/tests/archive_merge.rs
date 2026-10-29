mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn citation_fixture() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    project.initial();
    let claim = SemanticCitationRetentionAcquire {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        source_context: "knowledge:citations".into(),
        intent: SemanticCitationRetentionIntent {
            space_id: "space".into(),
            document_id: "doc".into(),
            publication_id: "pub".into(),
        },
        records: vec![SemanticRecordRef::element("a")],
        artifact_edits: Default::default(),
    };
    let plan = project
        .state
        .plan_citation_acquisition(&claim, "actor", &timestamp(2000), 2)
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    project
}

#[test]
fn generic_archive_import_cannot_mint_release_or_reauthorize_reserved_claims() {
    let project = citation_fixture();
    let original = project.state.clone();
    for mutation in ["higher", "release", "new", "receipt"] {
        let mut imported = original.clone();
        let head = imported
            .selections
            .iter_mut()
            .find(|head| {
                head.selection
                    .selection_id
                    .starts_with("knowledge-citation:")
            })
            .unwrap();
        match mutation {
            "higher" => head.selection.generation += 1,
            "release" => {
                head.selection.generation += 1;
                head.records.clear();
            }
            "new" => head.selection.selection_id.push_str("forged"),
            _ => imported
                .accepted_operations
                .last_mut()
                .unwrap()
                .request_digest
                .push_str("forged"),
        }
        assert!(
            original.merge_archive(&imported, 3000).is_err(),
            "{mutation}"
        );
        assert_eq!(project.state, original);
    }
    let mut before_claim = ProjectFixture::new();
    before_claim.initial();
    assert!(before_claim.state.merge_archive(&original, 3000).is_err());
    let normalized = original.merge_archive(&original, 3000).unwrap();
    assert_eq!(
        normalized.accepted_operations.len(),
        original.accepted_operations.len()
    );
    assert_eq!(
        original.merge_archive(&before_claim.state, 3000).unwrap(),
        normalized
    );
    let empty = ProjectFixture::new();
    assert_eq!(
        empty.state.merge_archive(&original, 3000).unwrap(),
        normalized
    );
}

#[test]
fn accepted_histories_from_different_project_identities_cannot_merge() {
    let mut first = ProjectFixture::new();
    first.initial();
    let mut second = ProjectFixture::new();
    second.state.project_id = "another-project".into();
    second.initial();
    let error = first.state.merge_archive(&second.state, 4000).unwrap_err();
    assert!(error.to_string().contains("existing project identity"));
}

#[test]
fn archive_restore_preserves_receipts_and_advances_only_a_compatible_history() {
    let mut project = ProjectFixture::new();
    project.initial();
    let earlier = project.state.clone();
    let edit = project.edit(
        "edit",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let accepted = project.publish(&edit);
    let restored = earlier.merge_archive(&project.state, 6000).unwrap();
    assert_eq!(restored, project.state);
    assert_eq!(restored.merge_archive(&earlier, 7000).unwrap(), restored);
    let plan = restored
        .plan_publication(&edit, &timestamp(8000), restored.commit_version + 1)
        .unwrap();
    assert_eq!(
        plan.outcome,
        SemanticRevisionPublicationOutcome::Accepted(accepted)
    );
    assert!(plan.accepted_operation.is_none());
}

#[test]
fn archive_restore_rejects_divergent_heads_and_different_bodies_under_the_same_id() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut fork = project.state.clone();
    let left = project.edit("left", vec![element("a", "a.rs", "A2")]);
    project.publish(&left);
    let mut right = left.clone();
    right.operation_id = "right".into();
    right.structure.as_mut().unwrap().elements[0].name = "different".into();
    let plan = fork
        .plan_publication(&right, &timestamp(3000), fork.commit_version + 1)
        .unwrap();
    fork.apply_publication(plan).unwrap();
    assert!(project.state.merge_archive(&fork, 5000).is_err());
    let mut corrupt = project.state.clone();
    let SemanticRevisionBody::Element(body) = &mut corrupt.records[0].body else {
        panic!("expected retained element");
    };
    body.name = "changed under existing id".into();
    assert!(project.state.merge_archive(&corrupt, 5000).is_err());
}

#[test]
fn archive_restore_can_restore_pruned_bytes_without_rewriting_current_membership() {
    let mut project = ProjectFixture::new();
    project.initial();
    let edit = project.edit(
        "edit",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    project.publish(&edit);
    let available = project.state.clone();
    project.collect();
    let restored = project.state.merge_archive(&available, 30_000).unwrap();
    assert_eq!(restored.selections, project.state.selections);
    let old = restored.resolve(&SemanticRecordRef::element("a")).unwrap();
    assert!(matches!(old.body, SemanticRevisionBody::Element(_)));
    assert_eq!(
        restored
            .records
            .iter()
            .find(|row| row.record == old.record)
            .unwrap()
            .unselected_at_ms,
        Some(30_000)
    );
}
