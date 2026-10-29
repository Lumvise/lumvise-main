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
        "content_size_bytes": null,
        "metadata": {"knowledge_document_assets": {"document_id": document.document_id}}}),
    )
}

pub(super) fn new_resource(
    context: &mut PluginContext<'_>,
    document: &mut KnowledgeDocument,
) -> Result<Option<Value>, PluginError> {
    if document.attachment_owner_id.as_deref() == Some(new_owner_id(&document.document_id).as_str())
    {
        let identity = context.host_call(
            "storage.semantic",
            json!({"operation":"project_identity", "project_root":document.project_root}),
        )?;
        let lineage = identity["identity"]["project_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                super::invalid(
                    "invalid_document_assets",
                    &identity.to_string(),
                    "a non-empty project lineage",
                )
            })?;
        document.attachment_owner_id = Some(format!(
            "document-assets:{lineage}:{}",
            hex::encode(&document.document_id)
        ));
    }
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

pub(super) fn for_update(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<Option<Value>, PluginError> {
    let Some(resource) = resource(document) else {
        return Ok(None);
    };
    let response = context.host_call(
        "storage.semantic",
        json!({"operation": "artifact", "artifact_id": document.attachment_owner_id}),
    )?;
    let stored = &response["artifact"];
    if stored.is_null()
        || (stored["metadata"]["knowledge_document_assets"]["document_id"] == document.document_id
            && stored["artifact_kind"] == "document_assets"
            && associated_resource(context, document, stored)?)
    {
        let mut resource = if stored.is_null() {
            resource
        } else {
            stored.clone()
        };
        resource
            .as_object_mut()
            .expect("stored resource object")
            .remove("project_root");
        resource["semantic_element_id"] = json!(document.semantic_element_id);
        return Ok(Some(resource));
    }
    Err(super::invalid(
        "document_assets_conflict",
        &stored.to_string(),
        "attachment resource owned by this document",
    ))
}

fn associated_resource(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    stored: &Value,
) -> Result<bool, PluginError> {
    let Some(owner) = crate::storage::semantic_element(context, &document.semantic_element_id)?
    else {
        return Ok(false);
    };
    if owner["project_root"] != document.project_root {
        return Ok(false);
    }
    let output = context.host_call("storage.semantic", json!({"operation":"artifacts_for_elements", "semantic_element_ids":[document.semantic_element_id]}))?;
    let artifacts = output["artifacts"].as_array().ok_or_else(|| {
        super::invalid(
            "invalid_document_assets",
            &output.to_string(),
            "current contextual artifact associations",
        )
    })?;
    Ok(artifacts
        .iter()
        .any(|artifact| artifact["artifact_id"] == stored["artifact_id"]))
}
