use super::common::*;
use lumvise_semantic_revisions::*;
use serde_json::{Value, json};

pub(super) fn cases() -> Vec<Vec<u8>> {
    let mut project = ProjectFixture::new();
    project.initial();
    let request = json!({"operation":"publication", "request":project.edit("advance", vec![element("a", "a.rs", "new")])});
    let mut cases = vec![];
    for request in [
        request,
        json!({"operation":"resolve","record":SemanticRecordRef::element("a")}),
        json!({"operation":"history"}),
        json!({"operation":"snapshot","selection_id":"working"}),
    ] {
        cases.push(serde_json::to_vec(&json!({"operation":"storage","request":{"action":"prepare","request_json":request.to_string(),"authenticated_actor":null}})).unwrap());
        cases.push(serde_json::to_vec(&json!({"operation":"storage","request":{"action":"execute","rows":[],"request_json":request.to_string(),"state_json":serde_json::to_string(&project.state).unwrap(),"authenticated_actor":null,"now_ms":"2000","published_at":"time","next_commit":"9007199254741007"}})).unwrap());
    }
    cases
}

#[test]
fn trusted_facts_are_bound_inside_rust_and_caller_facts_are_rejected() {
    let requests = cases();
    let output = String::from_utf8(evaluate_revision_policy(&requests[1])).unwrap();
    assert!(output.contains("9007199254741007"));
    for field in [
        "state",
        "basis",
        "authenticated_actor",
        "timestamp",
        "next_commit",
        "now_ms",
        "published_at",
    ] {
        let mut request: Value = serde_json::from_slice(&requests[1]).unwrap();
        let mut caller: Value =
            serde_json::from_str(request["request"]["request_json"].as_str().unwrap()).unwrap();
        caller[field] = json!("forged");
        request["request"]["request_json"] = json!(caller.to_string());
        let result: Value = serde_json::from_slice(&evaluate_revision_policy(
            &serde_json::to_vec(&request).unwrap(),
        ))
        .unwrap();
        assert_eq!(result["status"], "rejected", "{field}");
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("gateway-owned facts")
        );
    }
}
