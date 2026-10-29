//! Owns independent document identity, content, grouping and optional citations.
//!
//! Other plugins use the signed document exports. Graph representation and
//! lifecycle updates stay internal; no source-file anchor is required.

mod assets;
mod citations;
pub(crate) mod delegation;
mod generated;
mod hooks;
pub(crate) mod inspection;
pub(crate) mod maintenance;
mod publication;
mod record_format;
mod records;
mod reference_graph;
mod schema;
mod search;
mod spaces;
#[cfg(test)]
pub(crate) mod test_graph;
#[cfg(test)]
mod test_persistence;
#[cfg(test)]
mod tests;

use crate::semantic_context::SemanticElement;
use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, DeleteKnowledgeDocumentRequest, KnowledgeArtifactLifecycleV2,
    KnowledgeDocument, KnowledgeDocumentIdentity, ListKnowledgeDocumentsRequest,
    UpdateKnowledgeDocumentRequest,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(crate) struct IndexedDocumentPartition {
    pub(crate) source_elements: Vec<SemanticElement>,
    pub(crate) documents: Vec<KnowledgeDocument>,
    pub(crate) known_documents: Vec<KnowledgeDocument>,
}

/// Keep internal document index records out of source-only generation and projections.
/// Example: !is_document_index(&element.metadata) retains filesystem elements.
pub(crate) fn is_document_index(metadata: &Value) -> bool {
    records::is_document_record(metadata)
}

/// Rechecks frozen memory writes without changing their original editing base.
/// Example: the publication journal calls this before submitting a document batch.
pub(crate) fn authorize_publication(
    context: &mut PluginContext<'_>,
    batch: &Value,
) -> Result<(), PluginError> {
    spaces::authorize_publication(context, batch)
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
        known_documents: Vec::new(),
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
        if let Some(document) = records::decode(&record, artifacts, roots)? {
            self.known_documents.push(document.clone());
            if element.lifecycle != "inactive" && record_format::deleted(&record)?.is_none() {
                self.documents.push(document);
            }
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
    let mut documents = Vec::new();
    for id in ids {
        if let Some(document) = records::find(context, root, id)? {
            documents.push(document);
        }
    }
    Ok(documents)
}

pub(crate) use generated::is_generation_hold;
pub(crate) use generated::{
    DocumentGenerator, ObservedDocumentRevision, observe_generation_document,
    save as save_generated,
};
pub(crate) use publication::accepted_lifecycle_revision as publication_lifecycle_revision;
pub(crate) use publication::accepted_reference as publication_reference;
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
        "get_document_hook" => hooks::get(input, context),
        "configure_document_hook" => hooks::configure(input, context),
        "create_document" | "document.create" => create(input, context),
        "resolve_document_space" | "document.space" => spaces::resolve(input, context),
        "resolve_document_reference" | "document.reference" => citations::invoke(input, context),
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
    spaces::require_root(context, &request.project_root)?;
    let receipt = crate::publication_journal::execute(
        context,
        "document.create",
        &request,
        request.operation_id.as_deref(),
        |context, operation_id| {
            let identity = KnowledgeDocumentIdentity {
                project_root: request.project_root.clone(),
                document_id: request.document_id.clone(),
            };
            let deleted = match records::scoped_state(context, &identity)? {
                records::DocumentState::Stored(_) => {
                    return Err(invalid(
                        "document_exists",
                        &request.document_id,
                        "unused document identity",
                    ));
                }
                records::DocumentState::Deleted(document) => Some(document),
                records::DocumentState::Missing => None,
            };
            let expected_locator = deleted
                .as_ref()
                .map(|document| document.semantic_element_id.clone());
            let mut document = draft_document(request.clone());
            spaces::protect_write(context, &mut document)?;
            if let Some(deleted) = &deleted {
                document.semantic_element_id = deleted.semantic_element_id.clone();
            }
            crate::lifecycle::stamp(
                &mut document.lifecycle,
                deleted.as_ref().map(|document| &document.lifecycle),
                Utc::now(),
            );
            let resource = match deleted {
                Some(deleted) => {
                    document.attachment_owner_id = deleted.attachment_owner_id;
                    assets::for_update(context, &document)?
                }
                None => assets::new_resource(context, &mut document)?,
            };
            let resources = resource.into_iter().collect();
            publication::prepare(
                context,
                &document,
                resources,
                expected_locator.as_deref(),
                operation_id,
            )
            .map(Some)
        },
    )?;
    let document = publication::accepted(
        context,
        &request.project_root,
        &request.document_id,
        receipt.as_ref(),
    )?;
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
        access: None,
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

fn get(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let identity: KnowledgeDocumentIdentity = crate::parse(input, "document identity and project")?;
    let document = records::required(context, &identity)?;
    Ok(json!({"document": document}))
}

fn update(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    update_document(input, context, None)
}

fn update_document(
    input: Value,
    context: &mut PluginContext<'_>,
    delegation: Option<(&str, &[Vec<String>])>,
) -> Result<Value, PluginError> {
    let request: UpdateKnowledgeDocumentRequest =
        crate::parse(input, "revision-checked document update")?;
    spaces::require_root(context, &request.project_root)?;
    let intent = match delegation {
        Some((id, paths)) => json!({"request":request,"delegation":id,"causal_paths":paths}),
        None => json!(request),
    };
    let receipt = crate::publication_journal::execute(
        context,
        "document.update",
        &intent,
        request.operation_id.as_deref(),
        |context, operation_id| {
            let identity = KnowledgeDocumentIdentity {
                document_id: request.document_id.clone(),
                project_root: request.project_root.clone(),
            };
            let mut document = records::required(context, &identity)?;
            let previous = document.lifecycle.clone();
            if let Some((delegation, _)) = delegation {
                generated::require_memory_hook(context, &document, delegation)?;
            }
            apply_update(&mut document, request.clone())?;
            spaces::protect_write(context, &mut document)?;
            crate::lifecycle::stamp(&mut document.lifecycle, Some(&previous), Utc::now());
            if let Some((delegation, paths)) = delegation {
                maintenance::stamp(&mut document, paths)?;
                generated::stamp_memory_hook(&mut document, delegation);
            }
            let resources = assets::for_update(context, &document)?
                .into_iter()
                .collect();
            publication::prepare(
                context,
                &document,
                resources,
                Some(&document.semantic_element_id),
                operation_id,
            )
            .map(Some)
        },
    )?;
    let document = publication::accepted(
        context,
        &request.project_root,
        &request.document_id,
        receipt.as_ref(),
    )?;
    Ok(json!({"document": document}))
}

fn apply_update(
    document: &mut KnowledgeDocument,
    mut request: UpdateKnowledgeDocumentRequest,
) -> Result<(), PluginError> {
    validate_locator_revision(document, &request.expected_semantic_element_id)?;
    validate_revision(document, request.expected_revision)?;
    apply_text(document, &mut request)?;
    apply_context(document, &mut request)?;
    apply_status(document, request.status, request.superseded_by)
}

fn validate_locator_revision(
    document: &KnowledgeDocument,
    expected: &str,
) -> Result<(), PluginError> {
    if document.semantic_element_id == expected {
        return Ok(());
    }
    Err(invalid(
        "document_revision_conflict",
        expected,
        &format!(
            "selected locator {} for {}",
            document.semantic_element_id, document.document_id
        ),
    ))
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
    let request: DeleteKnowledgeDocumentRequest = crate::parse(
        input,
        "document deletion with the last-read expected_revision",
    )?;
    spaces::require_root(context, &request.project_root)?;
    crate::publication_journal::execute(
        context,
        "document.delete",
        &request,
        request.operation_id.as_deref(),
        |context, operation_id| {
            let identity = KnowledgeDocumentIdentity {
                document_id: request.document_id.clone(),
                project_root: request.project_root.clone(),
            };
            let mut document = records::required(context, &identity)?;
            validate_revision(&document, request.expected_revision)?;
            validate_locator_revision(&document, &request.expected_semantic_element_id)?;
            let previous = document.lifecycle.clone();
            crate::lifecycle::stamp(&mut document.lifecycle, Some(&previous), Utc::now());
            publication::prepare_remove(context, &document, operation_id).map(Some)
        },
    )?;
    Ok(json!({"document_id":request.document_id, "deleted":true}))
}

fn invalid(code: &str, value: &str, expected: &str) -> PluginError {
    PluginError::new(
        code,
        format!("invalid document value `{value}`; expected {expected}"),
        false,
    )
}
