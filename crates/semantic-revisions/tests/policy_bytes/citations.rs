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
    ]
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
