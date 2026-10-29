//! Project routes cannot retarget an editor's observed lineage while its work is in flight.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

#[test]
fn publication_rejects_a_different_lineage_even_when_the_observed_scope_is_identical() {
    let mut project = ProjectFixture::new();
    project.initial();
    let request = project.edit("stale-editor", vec![element("a", "a.rs", "changed")]);
    project.state.project_id = "another-project".into();
    let before = project.state.clone();
    let plan = project
        .state
        .plan_publication(&request, &timestamp(2000), 2)
        .unwrap();
    assert!(matches!(
        plan.outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    assert!(plan.accepted_operation.is_none());
    project.state.apply_publication(plan).unwrap();
    assert_eq!(project.state, before);
}

#[test]
fn selection_checks_observed_lineage_before_body_availability() {
    let project = ProjectFixture::new();
    let request = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "stale-selection".into(),
        project_root: ROOT.into(),
        project_id: "another-project".into(),
        selection_id: "working".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::FollowAccepted,
        records: vec![SemanticRecordRef::element("not-available")],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(&request, &timestamp(2000), 1)
        .unwrap();
    let SemanticRevisionPublicationOutcome::Conflict(conflict) = plan.outcome else {
        panic!("expected a project identity conflict");
    };
    assert_eq!(conflict.expected_project_id, "another-project");
    assert_eq!(conflict.actual_project_id, PROJECT_ID);
    assert!(plan.accepted_operation.is_none());
}

#[test]
fn an_empty_observed_identity_is_an_invalid_request_even_for_receipt_replay() {
    let mut project = ProjectFixture::new();
    let mut request = project.edit("initial", vec![element("a", "a.rs", "first")]);
    project.publish(&request);
    request.project_id.clear();
    let error = project
        .state
        .plan_publication(&request, &timestamp(2000), 2)
        .unwrap_err();
    assert!(error.to_string().contains("durable project identity"));
}
