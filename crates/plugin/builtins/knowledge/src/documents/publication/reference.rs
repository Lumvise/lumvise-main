//! Projects acknowledgement metadata from the frozen locator and its accepted identity.
use lumvise_contracts::{KnowledgeDocumentRevisionReference, SemanticRecordKind};
use lumvise_plugin_sdk::PluginError;
use lumvise_semantic_revisions::SemanticRevisionRemap;
use serde_json::Value;

/// Captures an exact write reference before the journal discards its frozen body.
/// Example: `accepted_reference(batch, receipt)` does not load current or historical payloads.
pub(crate) fn accepted_reference(
    batch: &Value,
    receipt: &Value,
) -> Result<Option<KnowledgeDocumentRevisionReference>, PluginError> {
    let mut reference = None;
    for element in batch["semantic_elements"].as_array().into_iter().flatten() {
        let Some(document_id) = document_identity(element) else {
            continue;
        };
        if reference.is_some() {
            return Err(invalid(
                element,
                "one logical document in a document publication",
            ));
        }
        reference = Some(reference_for_element(batch, receipt, element, document_id)?);
    }
    Ok(reference)
}

fn document_identity(element: &Value) -> Option<&str> {
    element["metadata"]["knowledge_document"]["document_id"]
        .as_str()
        .or_else(|| {
            element["metadata"]["knowledge_document_deleted"]["document"]["document_id"].as_str()
        })
}

/// Captures the original counter from the exact frozen document payload, never a current read.
/// Example: `accepted_lifecycle_revision(batch, receipt)` survives payload collection.
pub(crate) fn accepted_lifecycle_revision(
    batch: &Value,
    receipt: &Value,
) -> Result<u64, PluginError> {
    let element = batch["semantic_elements"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|element| document_identity(element).is_some())
        .ok_or_else(|| invalid(batch, "one frozen document locator"))?;
    let deleted = &element["metadata"]["knowledge_document_deleted"]["document"];
    if !deleted.is_null() {
        return lifecycle_counter(&deleted["revision"]);
    }
    let locator = &element["metadata"]["knowledge_document"];
    let body = batch["semantic_artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|body| {
            body["metadata"]["knowledge_document"]["document_id"] == locator["document_id"]
        })
        .ok_or_else(|| invalid(locator, "the exact frozen document body"))?;
    let incoming = body["artifact_id"]
        .as_str()
        .ok_or_else(|| invalid(body, "the frozen payload ID"))?;
    let accepted = crate::artifact_publication::accepted_artifact_id(incoming, Some(receipt))?;
    if locator["artifact_id"].as_str() != Some(accepted) {
        return Err(invalid(
            locator,
            "the accepted exact remap of the frozen document payload",
        ));
    }
    lifecycle_counter(&body["metadata"]["knowledge_document"]["revision"])
}

fn lifecycle_counter(value: &Value) -> Result<u64, PluginError> {
    value.as_u64().ok_or_else(|| {
        invalid(
            value,
            "the original nonnegative lifecycle counter of the frozen document",
        )
    })
}

fn reference_for_element(
    batch: &Value,
    receipt: &Value,
    element: &Value,
    document_id: &str,
) -> Result<KnowledgeDocumentRevisionReference, PluginError> {
    let incoming = element["semantic_element_id"]
        .as_str()
        .ok_or_else(|| invalid(element, "a frozen locator identity"))?;
    let root = batch["project_root"]
        .as_str()
        .ok_or_else(|| invalid(batch, "the original project root"))?;
    validated_document_reference(root, document_id, &accepted_locator_id(receipt, incoming)?)
}

fn accepted_locator_id(receipt: &Value, incoming: &str) -> Result<String, PluginError> {
    let remaps: Vec<SemanticRevisionRemap> = crate::parse(
        receipt["remaps"].clone(),
        "accepted document revision remaps",
    )?;
    let accepted = remaps
        .iter()
        .find(|remap| {
            remap.incoming.kind == SemanticRecordKind::Element
                && remap.incoming.record_id == incoming
        })
        .filter(|remap| remap.accepted.kind == SemanticRecordKind::Element)
        .ok_or_else(|| {
            invalid(
                receipt,
                "an exact accepted remap for the frozen document locator",
            )
        })?;
    Ok(accepted.accepted.record_id.clone())
}

fn validated_document_reference(
    root: &str,
    document_id: &str,
    accepted_id: &str,
) -> Result<KnowledgeDocumentRevisionReference, PluginError> {
    for (field, value) in [
        ("project_root", root),
        ("document_id", document_id),
        ("semantic_element_id", accepted_id),
    ] {
        if value.trim().is_empty() {
            return Err(invalid(&Value::String(value.into()), field));
        }
    }
    Ok(KnowledgeDocumentRevisionReference {
        project_root: root.into(),
        document_id: document_id.into(),
        semantic_element_id: accepted_id.into(),
    })
}

fn invalid(value: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_document_receipt",
        format!("document write reference `{value}`; expected {expected}"),
        false,
    )
}
