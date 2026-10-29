use super::{common::*, storage_envelopes::storage};
use serde_json::{Value, json};

fn candidates() -> Vec<Value> {
    let mut fixture = ProjectFixture::new();
    let elements = [
        ("z", "parse"),
        ("a", "parse"),
        ("b", "parse-file"),
        ("c", "unrelated"),
    ]
    .into_iter()
    .map(|(id, name)| {
        let mut element = element(id, &format!("{id}.rs"), "large private body");
        element.name = name.into();
        element
    })
    .collect();
    fixture.publish(&fixture.edit("seed", elements));
    let rows = storage(
        json!({"action":"rows","state_json":serde_json::to_string(&fixture.state).unwrap()}),
    );
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|row| row["key"]["kind"] == "record")
        .cloned()
        .map(|mut row| {
            row["body_json"] = Value::Null;
            row
        })
        .collect()
}

pub(super) fn cases() -> Vec<Vec<u8>> {
    let request =
        json!({"operation":"search_elements","selection_id":"working","query":" PARSE ","limit":2})
            .to_string();
    [
        json!({"action":"name_search_queries","request_json":request}),
        json!({"action":"name_search_selection","request_json":request,"rows":candidates()}),
    ]
    .into_iter()
    .map(|request| serde_json::to_vec(&json!({"operation":"storage","request":request})).unwrap())
    .collect()
}

#[test]
fn header_search_uses_shared_scores_limits_ties_and_no_bodies() {
    let request =
        json!({"operation":"search_elements","selection_id":"working","query":" PARSE ","limit":2})
            .to_string();
    let plan = storage(json!({"action":"name_search_queries","request_json":request}));
    assert_eq!(
        plan["queries"],
        json!([{"query":"index","name":"element_search_gram","value":"706172","selection_id":"working"}])
    );
    let result = storage(
        json!({"action":"name_search_selection","request_json":request,"rows":candidates()}),
    );
    let selected: Value = serde_json::from_str(result["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        selected["query"]["records"],
        json!([{"kind":"element","record_id":"a"},{"kind":"element","record_id":"z"}])
    );
    assert!(!result.to_string().contains("large private body"));
}
