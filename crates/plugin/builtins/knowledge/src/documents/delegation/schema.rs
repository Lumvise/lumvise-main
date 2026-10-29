//! Signed shape only; host authority validates the action and Knowledge owns its effect.
use super::super::schema::object;
use serde_json::{Value, json};

pub(crate) fn input_schema() -> Value {
    let text = json!({"type":"string","minLength":1});
    let revision = json!({"type":"integer","minimum":1});
    let reference = super::super::input_schema("create_document").expect("document schema")
        ["properties"]["references"]["items"].clone();
    object(
        &["delegation_id", "revision", "action"],
        json!({
        "delegation_id":text,"revision":revision,"action":{"oneOf":[
            object(&["action"], json!({"action":{"const":"read_destination"}})),
            object(&["action","reference"], json!({"action":{"const":"read_input"},"reference":reference})),
            object(&["action","operation_id"], json!({"action":{"const":"read_receipt"},"operation_id":text})),
            object(&["action","operation_id","expected_semantic_element_id","expected_revision","content"], json!({
                "action":{"const":"update_destination"},"operation_id":text,"expected_semantic_element_id":text,
                "expected_revision":revision,"content":{"type":"string"},
                "causal_paths":super::super::maintenance::schema::paths()}))
        ]}}),
    )
}

pub(crate) fn output_schema() -> Value {
    object(&["output"], json!({"output":{"type":"object"}}))
}
