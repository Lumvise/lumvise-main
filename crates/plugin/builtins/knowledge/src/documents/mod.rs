//! Owns independent document identity, content, grouping and optional citations.
//!
//! Other plugins use the signed document exports. Graph representation and
//! lifecycle updates stay internal; no source-file anchor is required.

mod records;
mod schema;
mod search;
#[cfg(test)]
mod tests;

use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeArtifactLifecycleV2, KnowledgeDocument,
    KnowledgeDocumentIdentity, ListKnowledgeDocumentsRequest, UpdateKnowledgeDocumentRequest,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(crate) use schema::{EXPORTS, description, input_schema, output_schema};

pub(crate) fn invoke(
    export_id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
    vectors: &crate::search::KnowledgeSearchVectors,
) -> Result<Value, PluginError> {
    match export_id {
        "create_document" | "document.create" => create(input, context),
        "get_document" | "document.get" => get(input, context),
        "update_document" | "document.update" => update(input, context),
        "list_documents" | "document.list" => list(input, context),
        "search_documents" | "document.search" => search::search(input, context, vectors),
        "delete_document" | "document.delete" => delete(input, context),
        other => Err(PluginError::unknown_capability(other)),
    }
}

fn create(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: CreateKnowledgeDocumentRequest = crate::parse(input, "independent document")?;
    records::validate_create(&request)?;
    if records::find(context, &request.document_id)?.is_some() {
        return Err(invalid(
            "document_exists",
            &request.document_id,
            "unused document identity",
        ));
    }
    let mut document = new_document(request);
    crate::lifecycle::stamp(&mut document.lifecycle, None, Utc::now());
    records::write(context, &document)?;
    Ok(json!({"document": document}))
}

fn new_document(request: CreateKnowledgeDocumentRequest) -> KnowledgeDocument {
    KnowledgeDocument {
        semantic_element_id: records::element_id(&request.document_id),
        document_id: request.document_id,
        project_root: request.project_root,
        document_kind: request.document_kind,
        title: request.title,
        content: request.content,
        path: request.path,
        tags: request.tags,
        references: request.references,
        metadata: request.metadata,
        lifecycle: KnowledgeArtifactLifecycleV2::default(),
    }
}

fn get(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let identity: KnowledgeDocumentIdentity = crate::parse(input, "document identity and project")?;
    let document = records::required(context, &identity)?;
    Ok(json!({"document": document}))
}

fn update(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: UpdateKnowledgeDocumentRequest =
        crate::parse(input, "revision-checked document update")?;
    let identity = KnowledgeDocumentIdentity {
        document_id: request.document_id.clone(),
        project_root: request.project_root.clone(),
    };
    let mut document = records::required(context, &identity)?;
    let previous = document.lifecycle.clone();
    apply_update(&mut document, request)?;
    crate::lifecycle::stamp(&mut document.lifecycle, Some(&previous), Utc::now());
    records::write(context, &document)?;
    Ok(json!({"document": document}))
}

fn apply_update(
    document: &mut KnowledgeDocument,
    mut request: UpdateKnowledgeDocumentRequest,
) -> Result<(), PluginError> {
    validate_revision(document, request.expected_revision)?;
    apply_text(document, &mut request)?;
    apply_context(document, &mut request)?;
    apply_status(document, request.status, request.superseded_by)
}

fn validate_revision(document: &KnowledgeDocument, expected: u64) -> Result<(), PluginError> {
    if expected != document.lifecycle.revision {
        return Err(invalid(
            "document_revision_conflict",
            &expected.to_string(),
            &format!(
                "stored revision {} for {}",
                document.lifecycle.revision, document.document_id
            ),
        ));
    }
    Ok(())
}

fn apply_text(
    document: &mut KnowledgeDocument,
    request: &mut UpdateKnowledgeDocumentRequest,
) -> Result<(), PluginError> {
    if let Some(title) = request.title.take() {
        crate::artifact::require(&title, "title")?;
        document.title = title;
    }
    if let Some(content) = request.content.take() {
        document.content = content;
    }
    if let Some(path) = request.path.take() {
        document.path = Some(path);
    }
    Ok(())
}

fn apply_context(
    document: &mut KnowledgeDocument,
    request: &mut UpdateKnowledgeDocumentRequest,
) -> Result<(), PluginError> {
    if let Some(tags) = request.tags.take() {
        document.tags = tags;
    }
    if let Some(references) = request.references.take() {
        records::validate_references(&references)?;
        document.references = references;
    }
    if let Some(metadata) = request.metadata.take() {
        document.metadata = metadata;
    }
    Ok(())
}

fn apply_status(
    document: &mut KnowledgeDocument,
    status: Option<lumvise_contracts::KnowledgeArtifactStatusV2>,
    superseded_by: Option<String>,
) -> Result<(), PluginError> {
    if let Some(status) = status {
        document.lifecycle.status = status;
        document.lifecycle.expires_at = None;
        if status != lumvise_contracts::KnowledgeArtifactStatusV2::Superseded {
            document.lifecycle.superseded_by = None;
        }
    }
    if let Some(target) = superseded_by {
        document.lifecycle.superseded_by = Some(target);
    }
    document
        .lifecycle
        .validate(&document.document_id)
        .map_err(|error| invalid("invalid_document_input", &document.document_id, &error))
}

fn list(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let request: ListKnowledgeDocumentsRequest = crate::parse(input, "document list filters")?;
    let documents = records::list(context, &request)?;
    Ok(json!({"documents": documents, "total": documents.len()}))
}

fn delete(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let identity: KnowledgeDocumentIdentity = crate::parse(input, "document identity and project")?;
    records::required(context, &identity)?;
    records::remove(context, &identity)?;
    Ok(json!({"document_id": identity.document_id, "deleted": true}))
}

fn invalid(code: &str, value: &str, expected: &str) -> PluginError {
    PluginError::new(
        code,
        format!("invalid document value `{value}`; expected {expected}"),
        false,
    )
}
