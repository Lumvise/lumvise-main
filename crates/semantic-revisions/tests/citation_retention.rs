//! Source-owned claims reuse pin policy without granting general selection mutation.
mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn citation() -> SemanticCitationRetentionAcquire {
    SemanticCitationRetentionAcquire {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        source_context: "git:main".into(),
        intent: SemanticCitationRetentionIntent {
            space_id: "private-space".into(),
            document_id: "decision".into(),
            publication_id: "save-decision".into(),
        },
        records: vec![SemanticRecordRef::element("a")],
        artifact_edits: Default::default(),
    }
}

fn release(request: &SemanticCitationRetentionAcquire) -> SemanticCitationRetentionRelease {
    SemanticCitationRetentionRelease {
        project_root: request.project_root.clone(),
        project_id: request.project_id.clone(),
        intent: request.intent.clone(),
        expected_generation: 1,
    }
}

fn acquire(
    project: &mut ProjectFixture,
    request: &SemanticCitationRetentionAcquire,
    actor: &str,
) -> SemanticRevisionPublicationOutcome {
    let plan = project
        .state
        .plan_citation_acquisition(
            request,
            actor,
            &timestamp(2000),
            project.state.commit_version + 1,
        )
        .unwrap();
    project.state.apply_publication(plan).unwrap()
}

#[test]
fn claim_identity_is_actor_lineage_and_document_intent_bound() {
    let request = citation();
    let identity = citation_retention_keys(PROJECT_ID, "reader", &request.intent).unwrap();
    for (lineage, actor) in [(PROJECT_ID, "other"), ("other-project", "reader")] {
        assert_ne!(
            identity,
            citation_retention_keys(lineage, actor, &request.intent).unwrap()
        );
    }
    let mut other = request.intent.clone();
    other.document_id = "another-document".into();
    assert_ne!(
        identity,
        citation_retention_keys(PROJECT_ID, "reader", &other).unwrap()
    );
    assert!(identity.selection_id.starts_with("knowledge-citation:"));
    assert!(!identity.selection_id.contains(&request.intent.document_id));
    assert!(citation_retention_keys(PROJECT_ID, "", &request.intent).is_err());
}

#[test]
fn acquisition_replays_after_serialization_but_rejects_changed_evidence() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = citation();
    let first = acquire(&mut project, &request, "reader");
    project.state = serde_json::from_slice(&serde_json::to_vec(&project.state).unwrap()).unwrap();
    assert_eq!(acquire(&mut project, &request, "reader"), first);
    request.records = vec![SemanticRecordRef::element("b")];
    assert!(
        project
            .state
            .plan_citation_acquisition(&request, "reader", &timestamp(3000), 99)
            .is_err()
    );
}

#[test]
fn only_original_claim_can_release_and_another_citation_stays_pinned() {
    let mut project = ProjectFixture::new();
    project.initial();
    let first = citation();
    let mut second = first.clone();
    second.intent.document_id = "second-document".into();
    acquire(&mut project, &first, "reader");
    acquire(&mut project, &second, "reader");
    let request = release(&first);
    assert!(
        project
            .state
            .plan_citation_release(&request, "other", &timestamp(3000), 99)
            .is_err()
    );
    let mut stale = request.clone();
    stale.expected_generation = 2;
    assert!(
        project
            .state
            .plan_citation_release(&stale, "reader", &timestamp(3000), 99)
            .is_err()
    );
    let plan = project
        .state
        .plan_citation_release(
            &request,
            "reader",
            &timestamp(3000),
            project.state.commit_version + 1,
        )
        .unwrap();
    let receipt = project.state.apply_publication(plan).unwrap();
    let replay = project
        .state
        .plan_citation_release(
            &request,
            "reader",
            &timestamp(4000),
            project.state.commit_version + 1,
        )
        .unwrap();
    assert_eq!(replay.outcome, receipt);
    let second_id = citation_retention_keys(PROJECT_ID, "reader", &second.intent)
        .unwrap()
        .selection_id;
    assert_eq!(
        project.state.selection(&second_id).unwrap().records,
        second.records
    );
    assert!(
        project
            .state
            .plan_citation_acquisition(&first, "reader", &timestamp(4000), 99)
            .is_err()
    );
}

#[test]
fn unknown_release_and_empty_acquisition_never_create_a_selection() {
    let project = ProjectFixture::new();
    let mut request = citation();
    assert!(
        project
            .state
            .plan_citation_release(&release(&request), "reader", &timestamp(1), 1)
            .is_err()
    );
    request.records.clear();
    assert!(
        project
            .state
            .plan_citation_acquisition(&request, "reader", &timestamp(1), 1)
            .is_err()
    );
    assert!(project.state.selections.is_empty());
}

#[test]
fn exact_working_evidence_requires_original_edit_token_and_freezes() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut edit = project.edit("add-note", vec![]);
    edit.structure = None;
    edit.artifacts.push(SemanticArtifactRevisionWrite {
        artifact: artifact("note", "a", "Evidence"),
        media_type: "text/plain".into(),
        checkpoint: false,
    });
    project.publish(&edit);
    let mut request = citation();
    request.records = vec![SemanticRecordRef::artifact("note")];
    request.artifact_edits.insert("note".into(), 0);
    let stale = project
        .state
        .plan_citation_acquisition(&request, "reader", &timestamp(2000), 99)
        .unwrap();
    assert!(matches!(
        stale.outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    request.artifact_edits.insert("note".into(), 1);
    acquire(&mut project, &request, "reader");
    assert!(
        project
            .state
            .resolve(&request.records[0])
            .unwrap()
            .artifact_state
            .unwrap()
            .frozen
    );
}

#[test]
fn generic_selection_publication_and_producer_base_cannot_mutate_claims() {
    let mut project = ProjectFixture::new();
    project.initial();
    let request = citation();
    acquire(&mut project, &request, "reader");
    let id = citation_retention_keys(PROJECT_ID, "reader", &request.intent)
        .unwrap()
        .selection_id;
    for mode in [
        SemanticSelectionMode::Pinned,
        SemanticSelectionMode::FollowAccepted,
    ] {
        let update = SemanticRevisionSelectionUpdate {
            operation_id: "bypass".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            selection_id: id.clone(),
            source_context: "git:main".into(),
            expected_generation: 1,
            artifact_edits: Default::default(),
            mode,
            records: vec![],
            connection_ids: vec![],
            source_anchor: None,
        };
        assert!(
            project
                .state
                .plan_selection(&update, &timestamp(3000), 99)
                .is_err()
        );
    }
    let mut edit = project.edit("bypass", vec![]);
    edit.selection_id = id.clone();
    assert!(
        project
            .state
            .plan_publication(&edit, &timestamp(3000), 99)
            .is_err()
    );
    edit.selection_id = "working".into();
    edit.producer_base = Some(SemanticProducerBase {
        selection_id: id,
        expected_generation: 1,
    });
    assert!(
        project
            .state
            .plan_publication(&edit, &timestamp(3000), 99)
            .is_err()
    );
}
