//! Static source I/O for sequencing tests; revision behavior uses the native boundary suites.
use super::SemanticElement;
use serde_json::{Value, json};

/// These fixtures contain one revision per source, so a chosen member is its exact ID.
/// Example: `selected_sources(&self.elements, &input)` in a named host-call fake.
pub(crate) fn selected_sources(elements: &[SemanticElement], input: &Value) -> Value {
    let requested = input["query"]["records"]
        .as_array()
        .expect("typed family query");
    let remaps = elements
        .iter()
        .filter(|element| element.lifecycle == "active")
        .filter(|element| element.project_root == input["query"]["project_root"])
        .filter_map(|element| {
            let record = json!({"kind":"element", "record_id":element.semantic_element_id});
            requested
                .contains(&record)
                .then(|| json!({"incoming":record,"accepted":record}))
        })
        .collect::<Vec<_>>();
    json!({"remaps":remaps})
}

#[test]
fn static_read_returns_only_requested_sources_in_the_declared_project() {
    let element = serde_json::from_value(json!({"project_root":"/project", "semantic_element_id":"a",
        "semantic_source_id":"files", "path":"a.rs", "element_kind":"file", "name":"A", "lifecycle":"active", "metadata":{}})).unwrap();
    let sources = [element];
    let mut input = json!({"query":{"project_root":"/project", "records":[{"kind":"element","record_id":"a"}, {"kind":"element","record_id":"absent"}]}});
    assert_eq!(
        selected_sources(&sources, &input)["remaps"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    input["query"]["project_root"] = json!("/other");
    assert_eq!(selected_sources(&sources, &input)["remaps"], json!([]));
}
