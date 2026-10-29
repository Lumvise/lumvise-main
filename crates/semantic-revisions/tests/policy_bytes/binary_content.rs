//! The real byte facade keeps pending content private and binds publication to its original writer.
use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn put() -> Value {
    json!({"operation":"blob_put","artifact_id":"binary","content_ref":"binary-ref","media_type":"application/octet-stream","content":[0,255,128,1]})
}

fn execute(state: &SemanticRevisionState, rows: &Value, request: &Value, actor: &str) -> Value {
    json!({"action":"execute","request_json":request.to_string(),"state_json":serde_json::to_string(state).unwrap(),"rows":rows,"authenticated_actor":actor,"now_ms":"2000","published_at":"2026-10-26T00:00:00Z","next_commit":(state.commit_version+1).to_string()})
}

fn fixture() -> (ProjectFixture, Value) {
    let mut project = ProjectFixture::new();
    let initial = project.edit("initial", vec![element("owner", "a.rs", "old")]);
    project.publish(&initial);
    let mut rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&project.state).unwrap()}),
    );
    let delta = storage(execute(&project.state, &rows, &put(), "writer"));
    rows.as_array_mut()
        .unwrap()
        .extend(delta["rows"].as_array().unwrap().clone());
    project.state.commit_version += 1;
    (project, rows)
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let (project, rows) = fixture();
    [
        execute(&project.state, &rows, &put(), "writer"),
        execute(
            &project.state,
            &rows,
            &json!({"operation":"blob","content_ref":"binary-ref"}),
            "writer",
        ),
    ]
    .into_iter()
    .map(|request| serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap())
    .collect()
}

fn rejected(request: Value) {
    let result: Value = serde_json::from_slice(&evaluate_revision_policy(
        &serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap(),
    ))
    .unwrap();
    assert_eq!(result["status"], "rejected", "{result}");
}

#[test]
fn staged_binary_replays_exactly_and_rejects_changed_bytes_actor_or_owner() {
    let (project, rows) = fixture();
    let replay = storage(execute(&project.state, &rows, &put(), "writer"));
    assert_eq!(replay["rows"], json!([]));
    assert_eq!(
        replay["commit_version"],
        project.state.commit_version.to_string()
    );
    for (field, value) in [
        ("content", json!([1])),
        ("media_type", json!("text/plain")),
        ("artifact_id", json!("other")),
    ] {
        let mut changed = put();
        changed[field] = value;
        rejected(execute(&project.state, &rows, &changed, "writer"));
    }
    rejected(execute(&project.state, &rows, &put(), "other-writer"));
    let hidden = storage(execute(
        &project.state,
        &rows,
        &json!({"operation":"blob","content_ref":"binary-ref"}),
        "reader",
    ));
    assert_eq!(hidden["value_json"], "null");
}

#[test]
fn publication_claims_exact_pending_owner_and_atomically_retires_staging() {
    let (mut project, rows) = fixture();
    let mut request = project.edit("publish-binary", vec![]);
    request.structure = None;
    let mut binary = artifact("binary", "owner", "searchable binary");
    binary.content = None;
    binary.content_ref = Some("binary-ref".into());
    binary.content_size_bytes = Some(4);
    request.artifacts.push(SemanticArtifactRevisionWrite {
        artifact: binary,
        media_type: "application/octet-stream".into(),
        checkpoint: false,
    });
    let input = json!({"operation":"publication","request":request});
    rejected(execute(&project.state, &rows, &input, "other-writer"));
    let mut forged = input.clone();
    forged["request"]["artifacts"][0]["artifact"]["artifact_id"] = json!("stolen");
    rejected(execute(&project.state, &rows, &forged, "writer"));
    let publication = storage(execute(&project.state, &rows, &input, "writer"));
    let plan = publication["value_json"].as_str().unwrap();
    let delta = storage(
        json!({"action":"publication","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":plan}),
    );
    let complete = storage(
        json!({"action":"contents","delta_json":json!({"kind":"delta","value":delta}).to_string(),"request_json":input.to_string(),"rows":rows}),
    );
    assert!(
        complete["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key["kind"] == "staged_blob")
    );
    let row = complete["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["key"]["kind"] == "record"
                && row["body_json"].as_str().unwrap().contains("binary-ref")
        })
        .unwrap();
    let body: Value = serde_json::from_str(row["body_json"].as_str().unwrap()).unwrap();
    assert_eq!(body["content"]["bytes"], json!([0, 255, 128, 1]));
    project.publish(&request);
}
