//! The private gateway envelope must preserve bytes and scope without moving revision rules to JavaScript.
use super::common::*;
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

pub(super) fn storage(request: Value) -> Value {
    let response = response(request);
    assert_eq!(response["status"], "accepted", "{response}");
    response["result"]["value"]["value"].clone()
}

fn response(request: Value) -> Value {
    serde_json::from_slice(&evaluate_revision_policy(
        &serde_json::to_vec(&json!({"operation":"storage", "request":request})).unwrap(),
    ))
    .unwrap()
}

fn exact_state() -> SemanticRevisionState {
    let mut project = ProjectFixture::new();
    let mut record = element("id\0with-nul", "src/file.rs", "one");
    record.start_line = Some(i64::MAX);
    record.metadata = json!({"signed":i64::MIN,"unsigned":u64::MAX});
    project.publish(&project.edit("initial", vec![record]));
    project.state.commit_version = 9_007_199_254_741_007;
    project.state.records[0].created_at_ms = 9_007_199_254_740_993;
    project.state
}

fn state_request(state: &SemanticRevisionState, rows: Value) -> Value {
    json!({"action":"state","project_root":ROOT,"project_id":PROJECT_ID,"commit_version":state.commit_version.to_string(),"rows":rows})
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let state = exact_state();
    let rows =
        storage(json!({"action":"rows","state_json":serde_json::to_string(&state).unwrap()}));
    let requests = vec![
        json!({"action":"evaluate", "request_json":String::from_utf8(super::large_integer_publication()).unwrap()}),
        json!({"action":"rows","state_json":serde_json::to_string(&state).unwrap()}),
        state_request(&state, rows),
    ];
    requests
        .into_iter()
        .map(|request| {
            serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap()
        })
        .collect()
}

#[test]
fn storage_envelopes_round_trip_i64_u64_and_nul_without_javascript_numbers() {
    let state = exact_state();
    let rows =
        storage(json!({"action":"rows","state_json":serde_json::to_string(&state).unwrap()}));
    for row in rows.as_array().unwrap() {
        assert!(row["metadata_json"].is_string());
        assert!(row["body_json"].is_null() || row["body_json"].is_string());
    }
    let decoded = storage(state_request(&state, rows));
    let raw = decoded["value_json"].as_str().unwrap();
    for exact in [
        "9007199254741007",
        "9007199254740993",
        "9223372036854775807",
        "-9223372036854775808",
        "18446744073709551615",
    ] {
        assert!(raw.contains(exact), "{raw}");
    }
    let round_trip: SemanticRevisionState = serde_json::from_str(raw).unwrap();
    assert_eq!(round_trip, state);
}

#[test]
fn storage_decode_rejects_foreign_lineage_missing_bodies_and_duplicate_rows() {
    let state = exact_state();
    let rows =
        storage(json!({"action":"rows","state_json":serde_json::to_string(&state).unwrap()}));
    let mut foreign = state_request(&state, rows.clone());
    foreign["project_id"] = json!("another-project");
    assert_eq!(response(foreign)["status"], "rejected");
    let mut missing = rows.clone();
    for row in missing.as_array_mut().unwrap() {
        row["body_json"] = Value::Null;
    }
    assert_eq!(
        response(state_request(&state, missing))["status"],
        "rejected"
    );
    let mut duplicated = rows.as_array().unwrap().clone();
    duplicated.push(duplicated[0].clone());
    assert_eq!(
        response(state_request(&state, json!(duplicated)))["status"],
        "rejected"
    );
    let mut forged = rows.clone();
    forged[0]["key"]["identity"] = json!("wrong");
    assert_eq!(
        response(state_request(&state, forged))["status"],
        "rejected"
    );
}

#[test]
fn scoped_storage_facts_do_not_require_unrelated_history_or_membership_bodies() {
    let mut project = ProjectFixture::new();
    project.publish(&project.edit(
        "initial",
        vec![element("a", "a.rs", "one"), element("b", "b.rs", "two")],
    ));
    let mut scoped = project.state.clone();
    scoped.selection_history.clear();
    scoped.accepted_operations.clear();
    scoped
        .records
        .retain(|record| record.record.record_id == "a");
    scoped.selections[0]
        .records
        .retain(|record| record.record_id == "a");
    let rows =
        storage(json!({"action":"rows","state_json":serde_json::to_string(&scoped).unwrap()}));
    let decoded = storage(state_request(&scoped, rows));
    let mut expected = scoped;
    expected.normalize_order();
    assert_eq!(
        serde_json::from_str::<SemanticRevisionState>(decoded["value_json"].as_str().unwrap())
            .unwrap(),
        expected
    );
    assert!(
        expected.validate_archive_state().is_err(),
        "partial facts must not masquerade as a full archive"
    );
}

#[test]
fn publication_envelope_emits_only_changed_rows_and_replay_emits_none() {
    let mut project = ProjectFixture::new();
    project.publish(&project.edit(
        "initial",
        vec![element("a", "a.rs", "one"), element("b", "b.rs", "two")],
    ));
    let request = project.edit(
        "changed",
        vec![element("a", "a.rs", "new"), element("b", "b.rs", "two")],
    );
    let plan = project.plan(&request);
    let change = storage(
        json!({"action":"publication","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":serde_json::to_string(&plan).unwrap()}),
    );
    let keys = change["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["key"])
        .collect::<Vec<_>>();
    assert!(
        !keys
            .iter()
            .any(|key| key["kind"] == "record"
                && key["identity"].as_str().unwrap().contains("\"b\""))
    );
    assert!(
        !keys.iter().any(|key| key["kind"] == "membership"
            && key["identity"].as_str().unwrap().contains("\"b\""))
    );
    assert_eq!(change["expected_commit"], "1");
    assert_eq!(change["commit_version"], "2");
    project.state.apply_publication(plan).unwrap();
    let replay = project.plan(&request);
    let change = storage(
        json!({"action":"publication","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":serde_json::to_string(&replay).unwrap()}),
    );
    assert_eq!(change["rows"], json!([]));
    assert_eq!(change["removed"], json!([]));
    assert_eq!(change["commit_version"], "2");
}

#[test]
fn storage_evaluation_preserves_raw_policy_result_and_rejects_recursion() {
    let request = super::large_integer_publication();
    let ordinary: Value = serde_json::from_slice(&evaluate_revision_policy(&request)).unwrap();
    let wrapped =
        storage(json!({"action":"evaluate","request_json":String::from_utf8(request).unwrap()}));
    assert_eq!(wrapped["kind"], ordinary["result"]["kind"]);
    assert_eq!(
        serde_json::from_str::<Value>(wrapped["value_json"].as_str().unwrap()).unwrap(),
        ordinary["result"]["value"]
    );
    let nested = json!({"operation":"storage","request":{"action":"rows","state_json":"{}"}});
    assert_eq!(
        response(json!({"action":"evaluate","request_json":nested.to_string()}))["status"],
        "rejected"
    );
}

#[test]
fn collection_envelope_keeps_history_and_removes_only_eligible_bodies() {
    let mut project = ProjectFixture::new();
    project.publish(&project.edit("initial", vec![element("a", "a.rs", "old")]));
    project.publish(&project.edit("advance", vec![element("a", "a.rs", "new")]));
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = project
        .state
        .plan_retention("collect", &policy, 20_000)
        .unwrap();
    let change = storage(
        json!({"action":"collection","state_json":serde_json::to_string(&project.state).unwrap(),"plan_json":serde_json::to_string(&plan).unwrap(),"policy_json":serde_json::to_string(&policy).unwrap(),"now_ms":"20000","next_commit":"3"}),
    );
    let tombstones = change["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["key"]["kind"] == "record" && row["body_json"].is_null())
        .count();
    assert_eq!(tombstones, 1);
    assert!(
        !change["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key["kind"] == "history" || key["kind"] == "operation")
    );
}
