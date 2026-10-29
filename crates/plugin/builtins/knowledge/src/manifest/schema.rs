//! Schema primitives hidden inside Knowledge's signed export manifest.

use serde_json::{Value, json};

pub(super) fn closed_object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "required": required, "properties": properties,
        "additionalProperties": false})
}

pub(super) fn knowledge_kind() -> Value {
    json!({"type": "string", "enum": ["requirements", "specification", "media"]})
}

pub(super) fn knowledge_status() -> Value {
    json!({"type": "string", "enum": ["active", "resolved", "superseded", "archived"]})
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
    lumvise_contracts::ArtifactDependency::input_schema()
}
