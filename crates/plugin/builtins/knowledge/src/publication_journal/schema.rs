//! Signed passive receipt interfaces; revision and authorization rules stay in the journal.
use serde_json::{Value, json};

pub(crate) const EXPORTS: &[(&str, &str)] = &[
    (
        "get_document_citation_retention",
        "Read document citation retention",
    ),
    (
        "reconcile_document_citations",
        "Reconcile obsolete citation claims",
    ),
    (
        "get_document_write_receipt",
        "Read document write acknowledgement",
    ),
    (
        "get_artifact_write_receipt",
        "Read artifact write acknowledgement",
    ),
];

pub(crate) fn description(id: &str) -> Option<&'static str> {
    if matches!(
        id,
        "get_document_citation_retention" | "reconcile_document_citations"
    ) {
        return Some(
            "Inspect source retention for an original document write. Reconciliation releases only this publication's claims after both its exact destination locator and payload are pruned. Current or retained historical documents keep their evidence. Pending writes can replay the identical request; conflicted writes conservatively retain their visible claims. This never discards another citation's claim or enables automatic collection.",
        );
    }
    EXPORTS.iter().any(|(export,_)| *export == id).then_some(
        "Read a known Knowledge write acknowledgement using a project_root resolving to its original project, the explicit operation_id and original create/update/delete action. Returns progress, exact accepted revision, lifecycle counter and acceptance version without loading retained content, following successors or retrying pending work.")
}

pub(crate) fn input_schema(id: &str) -> Option<Value> {
    description(id)?;
    Some(object(
        &["project_root", "operation_id", "action"],
        json!({
        "project_root":{"type":"string","minLength":1},"operation_id":{"type":"string","minLength":1},
        "action":{"type":"string","enum":["create","update","delete"]}}),
    ))
}

pub(crate) fn output_schema(id: &str) -> Option<Value> {
    if matches!(
        id,
        "get_document_citation_retention" | "reconcile_document_citations"
    ) {
        return Some(object(
            &["publication_id", "state", "claims"],
            json!({"publication_id":{"type":"string"},
            "state":{"enum":["not_found","pending","accepted","conflicted"]},
            "claims":{"type":"array", "items":object(&["project_root","state"], json!({"project_root":{"type":"string"},
                "state":{"enum":["pending","retained","released"]}}))}}),
        ));
    }
    let target = match id {
        "get_document_write_receipt" => super::receipts::ReceiptTarget::Document,
        "get_artifact_write_receipt" => super::receipts::ReceiptTarget::Artifact,
        _ => return None,
    };
    let mut schema = object(
        &[
            "operation_id",
            "publication_id",
            "project_root",
            "state",
            target.reference_field(),
            "lifecycle_revision",
            "commit_version",
        ],
        json!({
        "operation_id":{"type":"string"},"publication_id":{"type":"string"},"project_root":{"type":"string"},
        "state":{"type":"string","enum":["not_found","pending","accepted","conflicted"]},
        "lifecycle_revision":{"anyOf":[{"type":"null"},{"type":"integer","minimum":0}]},
        "commit_version":{"anyOf":[{"type":"null"},{"type":"integer","minimum":0}]}}),
    );
    schema["properties"][target.reference_field()] =
        json!({"anyOf":[{"type":"null"},reference_schema(target)]});
    Some(schema)
}

fn reference_schema(target: super::receipts::ReceiptTarget) -> Value {
    match target {
        super::receipts::ReceiptTarget::Document => object(
            &["document_id", "project_root", "semantic_element_id"],
            json!({
            "document_id":{"type":"string"},"project_root":{"type":"string"},"semantic_element_id":{"type":"string"}}),
        ),
        super::receipts::ReceiptTarget::Artifact => object(
            &["artifact_id", "project_root"],
            json!({
            "artifact_id":{"type":"string"},"project_root":{"type":"string"}}),
        ),
    }
}

fn object(required: &[&str], properties: Value) -> Value {
    json!({"type":"object","required":required,"properties":properties,"additionalProperties":false})
}
