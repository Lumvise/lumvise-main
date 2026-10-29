//! Producer pins protect offline editing bases without adopting unrelated accepted scopes.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn producer(selection_id: &str, generation: i64) -> Option<SemanticProducerBase> {
    Some(SemanticProducerBase {
        selection_id: selection_id.into(),
        expected_generation: generation,
    })
}

fn cooperating_project() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    for (id, pin) in [("attach-a", "producer:a"), ("attach-b", "producer:b")] {
        let mut request = project.edit(
            id,
            vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
        );
        request.producer_base = producer(pin, 0);
        let receipt = project.publish(&request);
        assert_eq!(receipt.producer_base.unwrap().generation, 1);
    }
    project
}

fn scoped_edit(
    project: &ProjectFixture,
    id: &str,
    path: &str,
    body: &str,
    pin: &str,
    generation: i64,
) -> SemanticRevisionPublication {
    let mut request = project.edit(
        id,
        vec![element(path.strip_suffix(".rs").unwrap(), path, body)],
    );
    path_edit(&mut request, path);
    request.producer_base = producer(pin, generation);
    request
}

#[test]
fn disjoint_acceptance_advances_only_the_producers_scope_and_protects_old_bodies() {
    let mut project = cooperating_project();
    let a = scoped_edit(&project, "a2", "a.rs", "A2", "producer:a", 1);
    let b = scoped_edit(&project, "b2", "b.rs", "B2", "producer:b", 1);
    let b2 = accepted_id(&project.publish(&b), "b");
    let a_receipt = project.publish(&a);
    let a2 = accepted_id(&a_receipt, "a");
    let pin = project.state.selection("producer:a").unwrap();
    let expected = [
        SemanticRecordRef::element(a2),
        SemanticRecordRef::element("b"),
    ]
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        pin.records
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert!(!pin.records.contains(&SemanticRecordRef::element(b2)));
    assert_eq!(pin.selection.generation, 2);
    assert_eq!(a_receipt.selection.generation, 4);
    assert_eq!(a_receipt.producer_base.as_ref().unwrap().generation, 2);
    let collected = project.collect();
    assert!(
        !collected
            .pruned_records
            .contains(&SemanticRecordRef::element("b"))
    );
    assert!(matches!(
        project
            .state
            .resolve(&SemanticRecordRef::element("b"))
            .unwrap()
            .body,
        SemanticRevisionBody::Element(_)
    ));
    assert_eq!(project.publish(&a), a_receipt);
}

#[test]
fn a_later_stale_edit_does_not_use_the_shared_head_as_its_untouched_base() {
    let mut project = cooperating_project();
    let mut a = scoped_edit(&project, "a2", "a.rs", "A2", "producer:a", 1);
    let b = scoped_edit(&project, "b2", "b.rs", "B2", "producer:b", 1);
    project.publish(&b);
    project.publish(&a);
    a = scoped_edit(&project, "a-stale-b", "b.rs", "B3", "producer:a", 2);
    a.expected.records = project
        .state
        .selection("producer:a")
        .unwrap()
        .records
        .clone();
    let before = project.state.clone();
    let plan = project.plan(&a);
    assert!(matches!(
        plan.outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    project.state.apply_publication(plan).unwrap();
    assert_eq!(project.state, before);
}

#[test]
fn producer_generation_conflicts_without_changing_the_shared_head() {
    let mut project = cooperating_project();
    let request = scoped_edit(&project, "a2", "a.rs", "A2", "producer:a", 1);
    project.publish(&request);
    let stale_pin = scoped_edit(&project, "a3", "a.rs", "A3", "producer:a", 1);
    let before = project.state.clone();
    let plan = project.plan(&stale_pin);
    assert!(
        matches!(&plan.outcome, SemanticRevisionPublicationOutcome::Conflict(conflict)
        if conflict.selection_id == "producer:a" && conflict.expected_generation == 1 && conflict.actual_generation == 2)
    );
    project.state.apply_publication(plan).unwrap();
    assert_eq!(project.state, before);
}

#[test]
fn an_existing_producer_pin_cannot_be_silently_recreated_or_retargeted() {
    let project = cooperating_project();
    let missing = scoped_edit(&project, "missing", "a.rs", "A2", "producer:missing", 2);
    assert!(matches!(
        project.plan(&missing).outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    for pin in ["", "working"] {
        let request = scoped_edit(&project, "invalid", "a.rs", "A2", pin, 0);
        assert!(
            project
                .state
                .plan_publication(
                    &request,
                    &timestamp(project.now),
                    project.state.commit_version + 1
                )
                .is_err()
        );
    }
    let mut wrong_context = scoped_edit(&project, "wrong-context", "a.rs", "A2", "producer:a", 1);
    wrong_context.source_context = "another-branch".into();
    wrong_context.selection_id = "branch-working".into();
    wrong_context.expected = Default::default();
    assert!(
        project
            .state
            .plan_publication(
                &wrong_context,
                &timestamp(project.now),
                project.state.commit_version + 1
            )
            .is_err()
    );
}

#[test]
fn releasing_producer_pins_makes_their_unneeded_bodies_collectable() {
    let mut project = cooperating_project();
    let request = scoped_edit(&project, "b2", "b.rs", "B2", "producer:b", 1);
    project.publish(&request);
    let pin = project
        .state
        .selection("producer:a")
        .unwrap()
        .selection
        .clone();
    let release = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "release-a".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: pin.selection_id,
        source_context: pin.source_context,
        expected_generation: pin.generation,
        mode: SemanticSelectionMode::Pinned,
        records: vec![],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(
            &release,
            &timestamp(project.now),
            project.state.commit_version + 1,
        )
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    let collected = project.collect();
    assert!(
        collected
            .pruned_records
            .contains(&SemanticRecordRef::element("b"))
    );
}
