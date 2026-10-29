//! Signed citation shapes reuse the existing public document/artifact/element records.
use super::super::schema::object;
use lumvise_contracts::{KnowledgeCitationTarget, KnowledgeQualifiedCitation};
use serde_json::{Value, json};

pub(in crate::documents) fn qualified() -> Value {
    KnowledgeQualifiedCitation::reference_schema()
}

pub(in crate::documents) fn read_input() -> Value {
    object(
        &["project_root", "target"],
        json!({"project_root":{"type":"string"}, "target":KnowledgeCitationTarget::input_schema()}),
    )
}

pub(in crate::documents) fn read_output() -> Value {
    object(
        &["reference", "state", "body"],
        json!({"reference":KnowledgeQualifiedCitation::input_schema(),
        "state":{"enum":["available", "missing", "pruned"]}, "body":{"anyOf":[
            {"type":"null"},
            object(&["target_kind", "element"], json!({"target_kind":{"const":"semantic_element"}, "element":crate::manifest::element_schema()})),
            object(&["target_kind", "artifact"], json!({"target_kind":{"const":"artifact"}, "artifact":crate::manifest::artifact_schema()})),
            object(&["target_kind", "document"], json!({"target_kind":{"const":"document"}, "document":super::super::schema::record()}))
        ]}}),
    )
}
