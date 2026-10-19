//! Binary attachment ownership uses the existing semantic artifact/blob boundary.
//! Resource records contain no document body and are outside the Knowledge namespace.

use lumvise_contracts::KnowledgeDocument;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(super) fn new_owner_id(document_id: &str) -> String {
    format!("document-assets:{}", hex::encode(document_id))
}

pub(super) fn resource(document: &KnowledgeDocument) -> Option<Value> {
    let owner = document.attachment_owner_id.as_ref()?;
    Some(
        json!({"artifact_id": owner, "semantic_element_id": document.semantic_element_id,
        "artifact_kind": "document_assets", "title": document.title,
        "content": null, "content_ref": null, "searchable_text": null,
        "content_size_bytes": null, "dependencies": [],
        "metadata": {"knowledge_document_assets": {"document_id": document.document_id}}}),
    )
}

pub(super) fn new_resource(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<Option<Value>, PluginError> {
    let Some(resource) = resource(document) else {
        return Ok(None);
    };
    let response = context.host_call(
        "storage.semantic",
        json!({"operation": "artifact",
        "artifact_id": document.attachment_owner_id}),
    )?;
    if !response["artifact"].is_null() {
        return Err(super::invalid(
            "document_assets_conflict",
            document.attachment_owner_id.as_deref().unwrap_or_default(),
            "unused attachment owner identity",
        ));
    }
    Ok(Some(resource))
}

pub(super) fn retire_legacy(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let artifact = resource(document).ok_or_else(|| {
        super::invalid(
            "invalid_document_assets",
            &document.document_id,
            "converted document attachment owner",
        )
    })?;
    let response = context.host_call(
        "storage.semantic",
        json!({"operation": "upsert_artifact",
        "artifact": artifact, "media_type": "application/octet-stream"}),
    )?;
    if response["artifact_id"].as_str() != document.attachment_owner_id.as_deref() {
        return Err(super::invalid(
            "invalid_document_assets",
            &response.to_string(),
            "stored attachment owner identity",
        ));
    }
    Ok(())
}
