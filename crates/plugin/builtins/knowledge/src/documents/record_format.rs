//! Internal document artifact format; public callers use the document exports.
//! The indexed locator selects one immutable payload, so a staged write is never a document.

use lumvise_contracts::KnowledgeDocument;
use lumvise_plugin_sdk::PluginError;
use serde_json::{Value, json};

pub(super) const PROVIDER: &str = "knowledge.documents";
pub(super) const PATH_PREFIX: &str = "@documents/";
pub(super) const ROOT_PATH: &str = "@documents/root";

pub(super) fn storage_path(document_id: &str) -> String {
    format!("@documents/{}", hex::encode(document_id))
}

pub(super) fn root_id(root: &str) -> String {
    format!("knowledge-documents:root:{}", hex::encode(root))
}

pub(super) fn source_id(root: &str) -> String {
    format!("knowledge-documents:{}", hex::encode(root))
}

pub(super) fn is_owned(metadata: &Value) -> bool {
    metadata["provider_instance_id"] == PROVIDER
        || !metadata["indexer_metadata"]["knowledge_document"].is_null()
        || !metadata["indexer_metadata"]["knowledge_document_root"].is_null()
}

pub(super) fn is_root(element: &Value) -> bool {
    element["metadata"]["provider_instance_id"] == PROVIDER
        && element["element_kind"] == "virtual_document_root"
        && element["path"] == ROOT_PATH
        && element["metadata"]["indexer_metadata"]["knowledge_document_root"]["schema"] == 1
}

pub(super) fn locator(element: &Value) -> Result<Option<&Value>, PluginError> {
    if !is_owned(&element["metadata"]) || is_root(element) || super::spaces::is_descriptor(element)
    {
        return Ok(None);
    }
    if deleted(element)?.is_some() {
        return Ok(None);
    }
    let locator = &element["metadata"]["indexer_metadata"]["knowledge_document"];
    if !valid_locator(element, locator) {
        return Err(super::invalid(
            "invalid_document_record",
            &element.to_string(),
            "a Knowledge document schema-2 artifact locator at its document storage path",
        ));
    }
    Ok(Some(locator))
}

pub(super) fn body_id(element: &Value) -> Option<&str> {
    element["metadata"]["indexer_metadata"]["knowledge_document"]["artifact_id"].as_str()
        .or_else(|| element["metadata"]["indexer_metadata"]["knowledge_document_deleted"]["previous_body_artifact_id"].as_str())
}

pub(super) fn deletion(document: &KnowledgeDocument, previous: &Value) -> Value {
    let mut marker = document.clone();
    marker.content.clear();
    marker.references.clear();
    marker.metadata = json!({"generation":{
        "owner":document.metadata["generation"]["owner"], "source_element_id":document.metadata["generation"]["source_element_id"]},
        "nucleus":{"target_element_id":document.metadata["nucleus"]["target_element_id"],
            "target_path":document.metadata["nucleus"]["target_path"]}});
    json!({"schema":1, "document":payload(&marker), "previous_body_artifact_id":body_id(previous)})
}

pub(super) fn deleted(element: &Value) -> Result<Option<KnowledgeDocument>, PluginError> {
    let marker = &element["metadata"]["indexer_metadata"]["knowledge_document_deleted"];
    if marker.is_null() {
        return Ok(None);
    }
    let mut payload = marker["document"].clone();
    let id = payload["document_id"].as_str().unwrap_or_default();
    if marker["schema"] != 1
        || payload["schema"] != 2
        || id.is_empty()
        || marker["previous_body_artifact_id"]
            .as_str()
            .is_none_or(str::is_empty)
        || ["content", "project_root", "semantic_element_id"]
            .iter()
            .any(|key| payload.get(key).is_some())
        || element["element_kind"] != "virtual_document"
        || element["path"] != storage_path(id)
        || element["metadata"]["provider_instance_id"] != PROVIDER
    {
        return Err(super::invalid(
            "invalid_document_record",
            &element.to_string(),
            "a project-owned document deletion marker",
        ));
    }
    payload
        .as_object_mut()
        .ok_or_else(|| invalid_body(element, marker))?
        .remove("schema");
    payload["content"] = json!("");
    payload["project_root"] = element["project_root"].clone();
    payload["semantic_element_id"] = element["semantic_element_id"].clone();
    crate::parse(
        payload,
        "a minimal document deletion identity and lifecycle",
    )
    .map(Some)
}

fn valid_locator(element: &Value, locator: &Value) -> bool {
    let id = locator["document_id"].as_str().unwrap_or_default();
    element["metadata"]["provider_instance_id"] == PROVIDER
        && element["element_kind"] == "virtual_document"
        && locator["schema"] == 2
        && !id.is_empty()
        && locator["artifact_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
        && element["path"] == storage_path(id)
}

pub(super) fn payload(document: &KnowledgeDocument) -> Value {
    let mut value = serde_json::to_value(document).expect("serializable document");
    let fields = value.as_object_mut().expect("document object");
    for field in ["content", "project_root", "semantic_element_id"] {
        fields.remove(field);
    }
    fields.insert("schema".into(), json!(2));
    value
}

pub(super) fn body_artifact(
    document: &KnowledgeDocument,
    anchor: &str,
    artifact_id: &str,
) -> Value {
    let payload = payload(document);
    json!({"artifact_id": artifact_id, "semantic_element_id": anchor,
        "artifact_kind": "knowledge_document", "title": document.title,
        "content": document.content, "content_ref": null, "searchable_text": null,
        "content_size_bytes": document.content.len(), "dependencies": document.attachment_owner_id.iter().map(|id|
            json!({"target":{"target_kind":"artifact", "artifact_id":id}})).collect::<Vec<_>>(),
        "metadata": {"knowledge_document": payload}})
}

pub(super) fn decode(
    element: &Value,
    artifact: &Value,
    roots: &[Value],
) -> Result<KnowledgeDocument, PluginError> {
    let locator = locator(element)?.ok_or_else(|| invalid_body(element, artifact))?;
    validate_body(element, artifact, roots, locator)?;
    let mut payload = artifact["metadata"]["knowledge_document"].clone();
    payload
        .as_object_mut()
        .expect("validated document payload")
        .remove("schema");
    payload["content"] = artifact["content"].clone();
    // Archive imports and remote adapters own root/ID remapping. Do not keep a second copy.
    payload["project_root"] = element["project_root"].clone();
    payload["semantic_element_id"] = element["semantic_element_id"].clone();
    crate::parse(payload, "complete stored Knowledge document artifact")
}

fn validate_body(
    element: &Value,
    artifact: &Value,
    roots: &[Value],
    locator: &Value,
) -> Result<(), PluginError> {
    if !body_owned_by_project(element, artifact, roots)
        || artifact["artifact_id"] != locator["artifact_id"]
        || artifact["artifact_kind"] != "knowledge_document"
        || !valid_body_payload(artifact, locator)
    {
        return Err(invalid_body(element, artifact));
    }
    Ok(())
}

fn body_owned_by_project(element: &Value, artifact: &Value, roots: &[Value]) -> bool {
    roots.iter().any(|root| {
        is_root(root)
            && root["lifecycle"] == "active"
            && root["project_root"] == element["project_root"]
            && root["semantic_element_id"] == artifact["semantic_element_id"]
    }) && artifact
        .get("project_root")
        .is_none_or(|root| root == &element["project_root"])
}

fn valid_body_payload(artifact: &Value, locator: &Value) -> bool {
    let payload = &artifact["metadata"]["knowledge_document"];
    payload["schema"] == 2
        && payload["document_id"] == locator["document_id"]
        && payload.get("access") == locator.get("access")
        && artifact["content"].is_string()
        && ["content", "project_root", "semantic_element_id"]
            .iter()
            .all(|key| payload.get(key).is_none())
}

fn invalid_body(element: &Value, artifact: &Value) -> PluginError {
    super::invalid(
        "invalid_document_record",
        &format!(
            "{} -> {}",
            element["semantic_element_id"], artifact["artifact_id"]
        ),
        "a complete schema-2 document artifact at this project's active document root",
    )
}
