use super::{common::*, storage_envelopes::storage};
use lumvise_semantic_revisions::SemanticElement;
use serde_json::{Value, json};

fn published_rows(elements: Vec<SemanticElement>) -> Vec<Value> {
    let mut fixture = ProjectFixture::new();
    fixture.publish(&fixture.edit("seed", elements));
    storage(json!({"action":"rows","state_json":serde_json::to_string(&fixture.state).unwrap()}))
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["key"]["kind"] == "record")
        .cloned()
        .collect()
}

fn candidates() -> Vec<Value> {
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
    published_rows(elements)
        .into_iter()
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

fn plan(query: &str, limit: usize) -> Value {
    let request =
        json!({"operation":"search_elements","selection_id":"working","query":query,"limit":limit})
            .to_string();
    storage(json!({"action":"name_search_queries","request_json":request}))["queries"].clone()
}

fn search_entries(row: &Value) -> Vec<(String, String)> {
    row["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|index| {
            index["name"]
                .as_str()
                .unwrap()
                .starts_with("element_search")
        })
        .map(|index| {
            (
                index["name"].as_str().unwrap().to_string(),
                index["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn header_search_uses_shared_scores_limits_ties_and_no_bodies() {
    let request =
        json!({"operation":"search_elements","selection_id":"working","query":" PARSE ","limit":2})
            .to_string();
    assert_eq!(
        plan(" PARSE ", 2),
        json!([{"query":"search","selection_id":"working","normalized":"parse","tokens":["%parse%"],"patterns":["%parse%"],"limit":2}])
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

#[test]
fn an_active_element_writes_one_lowercased_search_text_and_an_inactive_one_none() {
    let mut active = element("Src/Parse.RS:Fn", "Src/Parse.rs", "body");
    active.name = "Parse File".into();
    let mut inactive = element("Dormant", "dormant.rs", "body");
    inactive.lifecycle = "inactive".into();
    let rows = published_rows(vec![active, inactive]);
    let by_id = |id: &str| {
        rows.iter()
            .find(|row| row["key"]["identity"].as_str().unwrap().contains(id))
            .unwrap_or_else(|| panic!("row {id}"))
    };
    assert_eq!(
        search_entries(by_id("Src/Parse.RS:Fn")),
        [(
            "element_search_text".to_string(),
            "parse file\u{1f}src/parse.rs:fn".to_string()
        )]
    );
    assert_eq!(search_entries(by_id("Dormant")), []);
}

#[test]
fn nul_is_dropped_and_the_separator_replaced_so_the_text_keeps_exactly_one_split() {
    let mut odd = element("a\0B\u{1f}c", "odd.rs", "body");
    odd.name = "N\0\u{1f}X".into();
    let rows = published_rows(vec![odd]);
    assert_eq!(
        search_entries(&rows[0]),
        [(
            "element_search_text".to_string(),
            "n x\u{1f}ab c".to_string()
        )]
    );
}

#[test]
fn the_query_plan_escapes_like_characters_and_covers_the_query_and_every_token() {
    assert_eq!(
        plan(" 50%_Done\\Now ", 7),
        json!([{
            "query":"search","selection_id":"working","normalized":"50%_done\\now",
            "tokens":["%50%","%done%","%now%"],
            "patterns":["%50%","%50\\%\\_done\\\\now%","%done%","%now%"],
            "limit":7
        }])
    );
    // Short queries take the same shape: the adapter decides how to find them.
    assert_eq!(
        plan("zq", 3),
        json!([{"query":"search","selection_id":"working","normalized":"zq","tokens":["%zq%"],"patterns":["%zq%"],"limit":3}])
    );
    assert_eq!(plan("a\0", 3)[0]["normalized"], "a");
}

#[test]
fn nothing_is_requested_for_no_results_or_for_a_query_of_only_nul() {
    assert_eq!(plan("parse", 0), json!([]));
    assert_eq!(plan("\0\0", 5), json!([]));
}
