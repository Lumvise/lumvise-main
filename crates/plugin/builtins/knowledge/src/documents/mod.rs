//! Owns independent document identity, content, grouping and optional citations.
//!
//! Other plugins use the signed document exports. Graph representation and
//! lifecycle updates stay internal; no source-file anchor is required.

mod assets;
mod conversion;
mod generated;
mod publication;
mod record_format;
mod records;
mod reference_graph;
mod schema;
mod search;
#[cfg(test)]
pub(crate) mod test_graph;
#[cfg(test)]
mod tests;

use crate::semantic_context::SemanticElement;
use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeArtifactLifecycleV2, KnowledgeDocument,
    KnowledgeDocumentIdentity, ListKnowledgeDocumentsRequest, UpdateKnowledgeDocumentRequest,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(crate) struct IndexedDocumentPartition {
    pub(crate) source_elements: Vec<SemanticElement>,
    pub(crate) documents: Vec<KnowledgeDocument>,
}

/// Keep internal document index records out of source-only generation and projections.
/// Example: !is_document_index(&element.metadata) retains filesystem elements.
pub(crate) fn is_document_index(metadata: &Value) -> bool {
    records::is_document_record(metadata)
}

/// Internal payload/resources are consumed through documents, never as product Knowledge.
/// Example: snapshot.artifacts.retain(|row| !is_document_artifact(row)).
pub(crate) fn is_document_artifact(artifact: &Value) -> bool {
    matches!(
        artifact["artifact_kind"].as_str(),
        Some("knowledge_document" | "document_assets")
    ) || artifact["metadata"]["knowledge_document"].is_object()
        || artifact["metadata"]["knowledge_document_assets"].is_object()
}

/// Decode documents from an already captured revision and keep them out of source generation.
/// Example: partition_indexed_elements(root, snapshot.elements, &snapshot.artifacts) performs no live read.
pub(crate) fn partition_indexed_elements(
    root: &str,
    elements: Vec<SemanticElement>,
    artifacts: &[Value],
) -> Result<IndexedDocumentPartition, PluginError> {
    let mut partition = IndexedDocumentPartition {
        source_elements: Vec::new(),
        documents: Vec::new(),
    };
    let roots: Vec<Value> = elements.iter().map(|element| json!(element)).collect();
    for element in elements {
        partition.append(root, element, artifacts, &roots)?;
    }
    Ok(partition)
}

impl IndexedDocumentPartition {
    fn append(
        &mut self,
        root: &str,
        element: SemanticElement,
        artifacts: &[Value],
        roots: &[Value],
    ) -> Result<(), PluginError> {
        records::validate_element_scope(root, &element)?;
        if !records::is_document_record(&element.metadata) {
            self.source_elements.push(element);
            return Ok(());
        }
        let record = json!(element);
        if let Some(document) = records::decode(&record, artifacts, roots)?
            && element.lifecycle != "inactive"
        {
            self.documents.push(document);
        }
        Ok(())
    }
}

/// Resolve a targeted indexed page through the document owner, never through source metadata.
/// Example: partition_current_elements(context, root, changed_elements) keeps exact caller scope.
pub(crate) fn partition_current_elements(
    context: &mut PluginContext<'_>,
    root: &str,
    elements: Vec<SemanticElement>,
) -> Result<IndexedDocumentPartition, PluginError> {
    let (elements, artifacts) = records::captured_records(context, root, elements)?;
    partition_indexed_elements(root, elements, &artifacts)
}

/// Read a set of document identities within one exact project through the canonical owner.
/// Example: find_documents(context, root, &ids) is the batched equivalent of scoped document reads.
pub(crate) fn find_documents(
    context: &mut PluginContext<'_>,
    root: &str,
    ids: &std::collections::HashSet<String>,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    crate::artifact::require(root, "project_root")?;
    for id in ids {
        crate::artifact::require(id, "document_id")?;
    }
    let element_ids = ids.iter().map(|id| records::element_id(id)).collect();
    let elements = crate::storage::elements_by_ids(context, root, &element_ids)?;
    Ok(partition_current_elements(context, root, elements)?
        .documents
        .into_iter()
        .filter(|document| ids.contains(&document.document_id))
        .collect())
}

pub(crate) use generated::is_generation_hold;
pub(crate) use generated::{DocumentGenerator, save as save_generated};
pub(crate) use schema::{
    EXPORTS, description, input_schema, output_schema, record as record_schema,
};

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
        "convert_legacy_documents" => conversion::convert(input, context),
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
    let mut document = draft_document(request);
    crate::lifecycle::stamp(&mut document.lifecycle, None, Utc::now());
    records::create(context, &document)?;
    Ok(json!({"document": document}))
}

/// Build a document proposal without exposing its indexed representation.
/// Example: draft_document(request) assigns the Knowledge-owned index identity.
pub(crate) fn draft_document(request: CreateKnowledgeDocumentRequest) -> KnowledgeDocument {
    KnowledgeDocument {
        semantic_element_id: records::element_id(&request.document_id),
        attachment_owner_id: (request.document_kind
            == lumvise_contracts::KnowledgeDocumentKind::Canvas)
            .then(|| assets::new_owner_id(&request.document_id)),
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

/// Discover canonical source-document identities, including deleted ones, before automatic seeding.
/// Example: known_source_documents(context, root) prevents recreating an intentionally removed seed.
pub(crate) fn known_source_documents(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    crate::artifact::require(root, "project_root")?;
    records::project_document_records(context, root, true)
}

/// Read current document records through their owner, without exposing graph payloads.
/// Example: project_documents(context, root) supplies report refresh candidates.
pub(crate) fn project_documents(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    crate::artifact::require(root, "project_root")?;
    records::project_document_records(context, root, false)
}

/// Check source conversion readiness without decoding or adopting a retired body.
/// Example: conversion_required_for_source(context, root, source_id, "md-nucleus").
pub(crate) fn conversion_required_for_source(
    context: &mut PluginContext<'_>,
    root: &str,
    source_id: &str,
    tag: &str,
) -> Result<bool, PluginError> {
    conversion::required_for_source(context, root, source_id, tag)
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
    let document = records::required(context, &identity)?;
    if document.document_kind == lumvise_contracts::KnowledgeDocumentKind::Report
        && document.tags.iter().any(|tag| tag == "scoped-c4")
        && let Some(target_id) = document.metadata["nucleus"]["target_element_id"].as_str()
    {
        crate::cultivation::forget_c4_report_intent(&identity.project_root, target_id, context)?;
    }
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
