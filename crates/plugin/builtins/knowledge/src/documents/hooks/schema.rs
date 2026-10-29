use super::super::schema::object;
use serde_json::{Value, json};

pub(in crate::documents) fn input_schema(id: &str) -> Option<Value> {
    if id == "get_document_hook" {
        return Some(object(
            &["project_root", "document_id"],
            json!({"project_root":{"type":"string"},"document_id":{"type":"string"}}),
        ));
    }
    if id != "configure_document_hook" {
        return None;
    }
    Some(object(
        &[
            "operation_id",
            "project_root",
            "document_id",
            "expected_hook_revision",
            "enabled",
        ],
        json!({
        "operation_id":{"type":"string"},"project_root":{"type":"string"},"document_id":{"type":"string"},
        "expected_hook_revision":{"type":["integer","null"],"minimum":1},"enabled":{"type":"boolean"},
        "expected_revision":{"type":"integer","minimum":1},"expected_semantic_element_id":{"type":"string"},
        "settings":{"type":"object","description":"Agents settings: objective (text), propagation (existing static/dynamic/scheduled rule), optional schedule, optional linked_inputs. Omitted inputs watches all current direct links; [] watches the destination alone. Required when enabled."}}),
    ))
}

pub(in crate::documents) fn output_schema(id: &str) -> Option<Value> {
    matches!(id, "get_document_hook" | "configure_document_hook").then(|| {
        object(
            &["hook"],
            json!({
        "hook":{"type":["object","null"]},"document":super::super::schema::record()}),
        )
    })
}
