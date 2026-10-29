use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

fn fixture() -> ProjectFixture {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit("initial", vec![element("e", "file.rs", "old")]);
    initial.artifacts.push(SemanticArtifactRevisionWrite {
        artifact: artifact("a", "e", "search text"),
        checkpoint: false,
        media_type: "text/plain".into(),
    });
    project.publish(&initial);
    project
}

fn write(artifact: bool) -> Value {
    let vector = json!({"engine_id":"engine", "model":null, "dimensions":2,"vector":[0.5,-1.0],"normalized":false,"metadata":{"wide":u64::MAX}});
    if artifact {
        json!({"operation":"store_artifact_text_vectors","project_root":ROOT,"vectors":[{"artifact_id":"a","semantic_element_id":"e","source_text":"search text","vector":vector}]})
    } else {
        json!({"operation":"store_element_name_vectors","project_root":ROOT,"vectors":[{"project_root":ROOT,"semantic_element_id":"e","source_text":"e","vector":vector}]})
    }
}

fn execute(state: &SemanticRevisionState, request: Value, rows: Value) -> Value {
    json!({"action":"execute","request_json":request.to_string(),"state_json":serde_json::to_string(state).unwrap(),"rows":rows,"authenticated_actor":"owner","now_ms":"2000","published_at":"time","next_commit":"9007199254741007"})
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let project = fixture();
    let mut requests = vec![];
    for artifact in [false, true] {
        let request = write(artifact);
        requests.push(json!({"action":"prepare","request_json":request.to_string(),"authenticated_actor":"owner"}));
        let store = execute(&project.state, request, json!([]));
        let delta = storage(store.clone());
        requests.push(store);
        requests.push(execute(&project.state, json!({"operation":"vectors","selection_id":"working","artifact":artifact,"engine_id":"engine","model":null}), delta["rows"].clone()));
        requests.push(execute(&project.state, json!({"operation":"vectors","selection_id":"working","artifact":artifact,"engine_id":"engine","model":""}), delta["rows"].clone()));
    }
    requests
        .into_iter()
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        })
        .collect()
}

pub(super) fn rejected_cases() -> Vec<Vec<u8>> {
    let project = fixture();
    let mut cases = vec![];
    for artifact in [false, true] {
        for (field, value) in [
            ("dimensions", json!(0)),
            ("dimensions", json!(3)),
            ("vector", json!([1e300, 0.0])),
        ] {
            let mut invalid = write(artifact);
            invalid["vectors"][0]["vector"][field] = value;
            cases.push(execute(&project.state, invalid, json!([])));
        }
        let mut stale = write(artifact);
        stale["vectors"][0]["source_text"] = json!("stale text");
        cases.push(execute(&project.state, stale, json!([])));
    }
    let mut delta = storage(execute(&project.state, write(false), json!([])));
    delta["rows"][0]["body_json"] = json!(
        json!({"kind":"artifact","vectors":[{"artifact_id":"e","semantic_element_id":"e","source_text":"e","vector":write(false)["vectors"][0]["vector"]}]}).to_string()
    );
    cases.push(execute(
        &project.state,
        json!({"operation":"vectors","selection_id":"working","artifact":true,"engine_id":"engine","model":null}),
        delta["rows"].clone(),
    ));
    cases
        .into_iter()
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        })
        .collect()
}

#[test]
fn stored_vector_bytes_preserve_large_metadata_and_reject_invalid_batches() {
    for request in cases() {
        let response: Value = serde_json::from_slice(&evaluate_revision_policy(&request)).unwrap();
        assert_eq!(response["status"], "accepted", "{response}");
    }
    for request in rejected_cases() {
        let response: Value = serde_json::from_slice(&evaluate_revision_policy(&request)).unwrap();
        assert_eq!(response["status"], "rejected", "{response}");
    }
    let delta = storage(execute(&fixture().state, write(true), json!([])));
    assert_eq!(delta["commit_version"], "9007199254741007");
    assert!(
        delta["rows"][0]["body_json"]
            .as_str()
            .unwrap()
            .contains("18446744073709551615")
    );
}

#[test]
fn working_edits_and_collection_remove_vectors_in_the_same_commit() {
    let mut project = fixture();
    let mut request = project.edit("edit", vec![]);
    request.structure = None;
    request.artifacts.push(SemanticArtifactRevisionWrite {
        artifact: artifact("a", "e", "new text"),
        checkpoint: false,
        media_type: "text/plain".into(),
    });
    let delta = storage(
        json!({"action":"publication","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":serde_json::to_string(&project.plan(&request)).unwrap()}),
    );
    assert!(
        delta["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key["kind"] == "artifact_vector")
    );
    project.publish(&request);
    let mut remove = project.edit("remove", vec![]);
    remove.removed_artifact_ids.push("a".into());
    project.publish(&remove);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = project
        .state
        .plan_retention("sweep", &policy, 100_000)
        .unwrap();
    let delta = storage(
        json!({"action":"collection","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":serde_json::to_string(&plan).unwrap(),"policy_json":serde_json::to_string(&policy).unwrap(),"now_ms":"100000","next_commit":(project.state.commit_version+1).to_string()}),
    );
    assert!(
        delta["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key["kind"] == "element_vector")
    );
}

#[test]
fn vector_upsert_preserves_other_engines_and_replaces_only_the_same_model_key() {
    let project = fixture();
    for artifact in [false, true] {
        let first = storage(execute(&project.state, write(artifact), json!([])));
        let mut second = write(artifact);
        second["vectors"][0]["vector"]["engine_id"] = json!("second");
        let stored = storage(execute(&project.state, second, first["rows"].clone()));
        let mut replacement = write(artifact);
        replacement["vectors"][0]["vector"]["model"] = json!("");
        replacement["vectors"][0]["vector"]["vector"] = json!([1.0, 0.0]);
        let updated = storage(execute(&project.state, replacement, stored["rows"].clone()));
        let body: Value =
            serde_json::from_str(updated["rows"][0]["body_json"].as_str().unwrap()).unwrap();
        assert_eq!(body["vectors"].as_array().unwrap().len(), 2);
        assert_eq!(body["vectors"][0]["vector"]["vector"], json!([1.0, 0.0]));
        assert_eq!(body["vectors"][1]["vector"]["engine_id"], "second");
    }
}
