//! Private pure command shapes; no IO or permission fields are accepted.
use super::super::schema::{object, record};
use serde_json::{Value, json};

pub(crate) fn input_schema() -> Value {
    json!({"type":"object","additionalProperties":false,
        "properties":{"operation":{},"document":{},"content":{},"causal_paths":{}},"oneOf":[
        object(&["operation","document"],json!({"operation":{"const":"inspect"},"document":record()})),
        object(&["operation","document","content","causal_paths"],json!({"operation":{"const":"prepare"},
            "document":record(),"content":{"type":"string"},"causal_paths":paths()}))
    ]})
}

pub(crate) fn output_schema() -> Value {
    json!({"type":"object","additionalProperties":false,
        "properties":{"causal_paths":{},"metadata":{}},
        "oneOf":[object(&["causal_paths"],json!({"causal_paths":paths()})),
        object(&["metadata"],json!({"metadata":{"type":"object"}}))]})
}

pub(in crate::documents) fn paths() -> Value {
    json!({"type":"array","items":{"type":"array","items":{"type":"string","minLength":1},"uniqueItems":true}})
}
