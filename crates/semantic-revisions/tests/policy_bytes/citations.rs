use super::common::*;
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn fixture() -> (ProjectFixture, SemanticCitationRetentionAcquire) {
    let mut project = ProjectFixture::new();
    project.publish(&project.edit("seed", vec![element("evidence", "a.rs", "original")]));
    let request = SemanticCitationRetentionAcquire {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        source_context: "git:main".into(),
        intent: SemanticCitationRetentionIntent {
            space_id: "space".into(),
            document_id: "document".into(),
            publication_id: "save".into(),
        },
        records: vec![SemanticRecordRef::element("evidence")],
        artifact_edits: Default::default(),
    };
    (project, request)
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let (mut project, request) = fixture();
    let acquire = json!({"operation":"citation_acquisition","state":project.state,"request":request,"authenticated_actor":"verified-subject","timestamp":timestamp(2000),"next_commit":2});
    let plan = project
        .state
        .plan_citation_acquisition(&request, "verified-subject", &timestamp(2000), 2)
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    let release = SemanticCitationRetentionRelease {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        intent: request.intent,
        expected_generation: 1,
    };
    let release = json!({"operation":"citation_release","state":project.state,"request":release,"authenticated_actor":"verified-subject","timestamp":timestamp(3000),"next_commit":3});
    vec![
        serde_json::to_vec(&acquire).unwrap(),
        serde_json::to_vec(&release).unwrap(),
        unavailable_resolution_case(),
    ]
}

fn unavailable_resolution_case() -> Vec<u8> {
    let (project, record, _) = unavailable_successor_fixture();
    serde_json::to_vec(&json!({
        "operation":"resolve",
        "state":project.state,
        "record":record
    }))
    .unwrap()
}

pub(super) fn acknowledged_loss_fixture() -> ProjectFixture {
    let mut project = available_evidence_fixture();
    recover_evidence(&mut project);
    project
}

pub(super) fn available_evidence_fixture() -> ProjectFixture {
    initial_evidence_fixture()
}

pub(super) fn recover_evidence(project: &mut ProjectFixture) {
    let report = recovery_report(&project.state);
    let recovery = recovery_request(&project.state);
    apply_recovery(project, report, recovery);
}

fn initial_evidence_fixture() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    let mut original = element("evidence", "a.rs", "historical bytes");
    original.metadata["provider_instance_id"] = "source-parser".into();
    let healthy = element("healthy", "healthy.rs", "healthy selected bytes");
    let initial = project.edit("initial-evidence", vec![original, healthy]);
    project.publish(&initial);
    project
}

fn recovery_report(state: &SemanticRevisionState) -> SemanticRevisionIntegrityReport {
    let header = state
        .records
        .iter()
        .find(|record| record.record.record_id == "evidence")
        .map(SemanticRevisionRecordHeader::from)
        .unwrap();
    SemanticRevisionIntegrityReport {
        publication_inspection: None,
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        commit_version: state.commit_version,
        missing_records: vec![header],
    }
}

fn recovery_request(state: &SemanticRevisionState) -> SemanticRevisionRecoveryRequest {
    SemanticRevisionRecoveryRequest {
        operation_id: "acknowledge-loss".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        expected_commit_version: state.commit_version,
        provider_instance_id: "source-parser".into(),
        missing_records: vec![SemanticRecordRef::element("evidence")],
    }
}

fn apply_recovery(
    project: &mut ProjectFixture,
    report: SemanticRevisionIntegrityReport,
    recovery: SemanticRevisionRecoveryRequest,
) {
    let plan = report
        .plan_recovery(&recovery, project.now, project.state.commit_version + 1)
        .unwrap();
    apply_recovery_plan(project, plan);
}

fn apply_recovery_plan(project: &mut ProjectFixture, plan: SemanticRevisionRecoveryPlan) {
    let unavailable = plan
        .headers
        .iter()
        .find(|header| header.record.record_id == "evidence")
        .unwrap()
        .clone();
    let record_index = project
        .state
        .records
        .iter()
        .position(|record| record.record.record_id == "evidence")
        .unwrap();
    project.state.records[record_index] = unavailable
        .clone()
        .restore(unavailable.payload_absence().unwrap())
        .unwrap();
    project.state.commit_version = plan.accepted.receipt.commit_version;
    project.state.recovery_operations.push(plan.accepted);
}

pub(super) fn pin_working_selection(project: &mut ProjectFixture) {
    let update = working_pin_update();
    let plan = project
        .state
        .plan_selection(
            &update,
            &timestamp(project.now),
            project.state.commit_version + 1,
        )
        .unwrap();
    assert!(matches!(
        project.state.apply_publication(plan).unwrap(),
        SemanticRevisionPublicationOutcome::Accepted(_)
    ));
    project.now += 1000;
}

fn working_pin_update() -> SemanticRevisionSelectionUpdate {
    SemanticRevisionSelectionUpdate {
        operation_id: "pin-retained-working".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "working".into(),
        source_context: "git:main".into(),
        expected_generation: 1,
        artifact_edits: Default::default(),
        mode: SemanticSelectionMode::Pinned,
        records: vec![
            SemanticRecordRef::element("evidence"),
            SemanticRecordRef::element("healthy"),
        ],
        connection_ids: vec![],
        source_anchor: None,
    }
}

pub(super) fn publish_rescan_successor(project: &mut ProjectFixture) {
    project.now += 1000;
    let mut rescan = project.edit(
        "rescan-evidence",
        vec![element("evidence", "a.rs", "freshly scanned bytes")],
    );
    rescan.selection_id = "rescan".into();
    rescan.expected = SemanticSelectionExpectation::default();
    path_edit(&mut rescan, "a.rs");
    project.publish(&rescan);
}

fn unavailable_successor_fixture() -> (ProjectFixture, SemanticRecordRef, String) {
    let mut project = acknowledged_loss_fixture();
    let successor_id = publish_unavailable_successor(&mut project);
    (
        project,
        SemanticRecordRef::element("evidence"),
        successor_id,
    )
}

pub(super) fn publish_unavailable_successor(project: &mut ProjectFixture) -> String {
    let mut replacement = element("evidence", "a.rs", "freshly scanned bytes");
    replacement.metadata["provider_instance_id"] = "source-parser".into();
    let mut rescan = project.edit("rescan-evidence", vec![replacement]);
    path_edit(&mut rescan, "a.rs");
    let receipt = project.publish(&rescan);
    accepted_id(&receipt, "evidence")
}

#[test]
fn byte_claims_bind_actor_and_preserve_empty_release_receipts() {
    let requests = cases();
    let released: Value = serde_json::from_slice(&evaluate_revision_policy(&requests[1])).unwrap();
    assert_eq!(released["status"], "accepted");
    let receipt = &released["result"]["value"]["outcome"]["value"];
    assert_eq!(receipt["accepted_scope"]["records"], json!([]));
    assert_eq!(receipt["remaps"], json!([]));
    let mut stranger: Value = serde_json::from_slice(&requests[1]).unwrap();
    stranger["authenticated_actor"] = json!("another-actor");
    let denied: Value = serde_json::from_slice(&evaluate_revision_policy(
        &serde_json::to_vec(&stranger).unwrap(),
    ))
    .unwrap();
    assert_eq!(denied["status"], "rejected");
}

#[test]
fn unavailable_exact_citation_never_returns_cached_or_successor_bytes() {
    let (project, record, successor_id) = unavailable_successor_fixture();
    let resolution = project.state.resolve(&record).unwrap();
    assert!(matches!(
        resolution.body,
        SemanticRevisionBody::Unavailable(_)
    ));
    assert_eq!(resolution.successors.len(), 1);
    assert_eq!(resolution.successors[0].newer.record_id, successor_id);
    let successor = project
        .state
        .records
        .iter()
        .find(|stored| stored.record.record_id == successor_id)
        .unwrap();
    let SemanticRevisionBody::Element(successor_body) = &successor.body else {
        panic!("expected the accepted successor to retain its own element body");
    };
    assert_eq!(successor_body.metadata["body"], "freshly scanned bytes");

    let acquisition = SemanticCitationRetentionAcquire {
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        source_context: "git:main".into(),
        intent: SemanticCitationRetentionIntent {
            space_id: "space".into(),
            document_id: "document".into(),
            publication_id: "save-unavailable".into(),
        },
        records: vec![record.clone()],
        artifact_edits: Default::default(),
    };
    assert!(
        project
            .state
            .plan_citation_acquisition(
                &acquisition,
                "verified-subject",
                &timestamp(4000),
                project.state.commit_version + 1,
            )
            .is_err()
    );

    let request = serde_json::to_vec(&json!({
        "operation":"resolve",
        "state":&project.state,
        "record":&record
    }))
    .unwrap();
    let response: Value = serde_json::from_slice(&evaluate_revision_policy(&request)).unwrap();
    let exact = &response["result"]["value"];
    assert_eq!(response["status"], "accepted");
    assert_eq!(exact["body"]["state"], "unavailable");
    assert_eq!(
        exact["body"]["record"]["recovery_operation_id"],
        "acknowledge-loss"
    );
    assert!(exact["body"]["record"].get("metadata").is_none());
    assert_eq!(exact["successors"][0]["newer"]["record_id"], successor_id);
}
