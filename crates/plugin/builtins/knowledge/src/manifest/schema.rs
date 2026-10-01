//! Schema primitives hidden inside Knowledge's signed export manifest.

use serde_json::{Value, json};

pub(super) fn closed_object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "required": required, "properties": properties,
        "additionalProperties": false})
}

pub(super) fn knowledge_kind() -> Value {
    json!({"type": "string", "enum": ["specification", "issue", "task_assignment",
        "definition", "annotation", "report", "decision", "manual_note", "derived_summary"]})
}

pub(super) fn nullable(schema: Value) -> Value {
    json!({"anyOf": [schema, {"type": "null"}]})
}

pub(super) fn string() -> Value {
    json!({"type": "string"})
}

pub(super) fn integer() -> Value {
    json!({"type": "integer"})
}

pub(super) fn number() -> Value {
    json!({"type": "number"})
}

pub(super) fn boolean() -> Value {
    json!({"type": "boolean"})
}

pub(super) fn array(items: Value) -> Value {
    json!({"type": "array", "items": items})
}
pub(super) fn artifact_dependency() -> Value {
    json!({"type": "object", "required": ["target"], "additionalProperties": false,
    "properties": {"target": {"oneOf": [
        closed_object(&["target_kind", "semantic_element_id"],
            json!({"target_kind": {"const": "semantic_element"}, "semantic_element_id": string()})),
        closed_object(&["target_kind", "artifact_id"],
            json!({"target_kind": {"const": "artifact"}, "artifact_id": string()}))
    ]}}})
}
