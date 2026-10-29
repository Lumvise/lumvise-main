//! Closed passive citation shapes shared by signed manifests. No resolution or retention policy.
use super::{KnowledgeCitationTarget, KnowledgeProjectIdentity, KnowledgeQualifiedCitation};
use serde_json::{Value, json};

impl KnowledgeProjectIdentity {
    /// Describes the authoritative project identity carried by citations and memory restrictions.
    /// Example: `KnowledgeProjectIdentity::input_schema()` for a required project field.
    pub fn input_schema() -> Value {
        object(
            &["project_id", "project_root", "storage_authority"],
            json!({
                "project_id":{"type":"string"}, "project_root":{"type":"string"}, "storage_authority":{"type":"string"}
            }),
        )
    }
}

impl KnowledgeQualifiedCitation {
    /// Describes one canonical citation without choosing or reading its target.
    /// Example: `KnowledgeQualifiedCitation::input_schema()` for a resolver response.
    pub fn input_schema() -> Value {
        object(
            &["project", "target"],
            json!({"project":KnowledgeProjectIdentity::input_schema(),
            "target":KnowledgeCitationTarget::input_schema()}),
        )
    }

    /// Describes the qualified variant of a document reference or linked hook input.
    /// Example: `KnowledgeQualifiedCitation::reference_schema()` in a reference union.
    pub fn reference_schema() -> Value {
        let mut schema = Self::input_schema();
        schema["required"] = json!(["target_kind", "project", "target"]);
        schema["properties"]["target_kind"] = json!({"const":"qualified"});
        schema
    }
}

impl KnowledgeCitationTarget {
    /// Describes typed exact/selected targets while policy remains in Knowledge and shared revisions.
    /// Example: `KnowledgeCitationTarget::input_schema()` for a resolver request.
    pub fn input_schema() -> Value {
        json!({"oneOf":[
            object(&["target_kind", "semantic_element_id", "selection"], json!({"target_kind":{"const":"semantic_element"},
                "semantic_element_id":{"type":"string"}, "selection":revision_selection()})),
            object(&["target_kind", "artifact_id", "selection"], json!({"target_kind":{"const":"artifact"},
                "artifact_id":{"type":"string"}, "edit_version":{"anyOf":[{"type":"integer","minimum":1},{"type":"null"}]},
                "selection":revision_selection()})),
            object(&["target_kind", "document_id", "selection"], json!({"target_kind":{"const":"document"},
                "document_id":{"type":"string"}, "selection":{"oneOf":[
                    object(&["mode"], json!({"mode":{"const":"current"}})),
                    object(&["mode", "semantic_element_id", "source_context"], json!({"mode":{"const":"exact"},
                        "semantic_element_id":{"type":"string"}, "source_context":{"type":"string"}}))
                ]}}))
        ]})
    }
}

fn revision_selection() -> Value {
    json!({"oneOf":[
        object(&["mode", "selection_id", "source_context"], json!({"mode":{"const":"selected"},
            "selection_id":{"type":"string"}, "source_context":{"type":"string"}})),
        object(&["mode", "source_context"], json!({"mode":{"const":"exact"}, "source_context":{"type":"string"}}))
    ]})
}

fn object(required: &[&str], properties: Value) -> Value {
    json!({"type":"object", "required":required, "properties":properties, "additionalProperties":false})
}
