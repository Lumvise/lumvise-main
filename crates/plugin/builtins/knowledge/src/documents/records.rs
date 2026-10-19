//! Single graph representation for Knowledge-owned independent documents.

use std::collections::HashSet;

use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeDocument, KnowledgeDocumentIdentity,
    KnowledgeDocumentReference, ListKnowledgeDocumentsRequest,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

const PROVIDER: &str = "knowledge.documents";

pub(super) fn element_id(document_id: &str) -> String {
    format!("knowledge-document:{document_id}")
}

pub(super) fn validate_create(request: &CreateKnowledgeDocumentRequest) -> Result<(), PluginError> {
    crate::artifact::require(&request.document_id, "document_id")?;
    crate::artifact::require(&request.project_root, "project_root")?;
    crate::artifact::require(&request.title, "title")?;
    validate_references(&request.references)
}

pub(super) fn validate_references(
    references: &[KnowledgeDocumentReference],
) -> Result<(), PluginError> {
    let mut seen = HashSet::new();
    for reference in references {
        let id = reference_id(reference);
        crate::artifact::require(id, "reference target")?;
        if !seen.insert(reference) {
            return Err(super::invalid(
                "invalid_document_input",
                id,
                "unique document references",
            ));
        }
    }
    Ok(())
}

fn reference_id(reference: &KnowledgeDocumentReference) -> &str {
    match reference {
        KnowledgeDocumentReference::SemanticElement {
            semantic_element_id,
        } => semantic_element_id,
        KnowledgeDocumentReference::Artifact { artifact_id } => artifact_id,
        KnowledgeDocumentReference::Document { document_id } => document_id,
    }
}

pub(super) fn find(
    context: &mut PluginContext<'_>,
    document_id: &str,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    let element = crate::storage::semantic_element(context, &element_id(document_id))?;
    match element {
        Some(element) if element["lifecycle"] != "inactive" => decode(&element),
        _ => Ok(None),
    }
}

pub(super) fn required(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<KnowledgeDocument, PluginError> {
    crate::artifact::require(&identity.document_id, "document_id")?;
    crate::artifact::require(&identity.project_root, "project_root")?;
    find_scoped(context, identity)?
        .filter(|document| document.project_root == identity.project_root)
        .ok_or_else(|| {
            super::invalid(
                "document_not_found",
                &identity.document_id,
                "document in the requested project",
            )
        })
}

fn find_scoped(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    let elements = crate::storage::elements_by_ids(
        context,
        &identity.project_root,
        &HashSet::from([element_id(&identity.document_id)]),
    )?;
    for element in elements {
        if element.lifecycle == "inactive" {
            continue;
        }
        let record = serde_json::to_value(element).expect("serializable semantic element");
        if let Some(document) = decode(&record)?
            && document.document_id == identity.document_id
        {
            return Ok(Some(document));
        }
    }
    Ok(None)
}

pub(super) fn find_for_conversion(
    context: &mut PluginContext<'_>,
    root: &str,
    document_id: &str,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    let elements =
        crate::storage::elements_by_ids(context, root, &HashSet::from([element_id(document_id)]))?;
    let element = elements
        .first()
        .map(|element| serde_json::to_value(element).expect("serializable semantic element"));
    let Some(element) = element else {
        return find(context, document_id);
    };
    if element["lifecycle"] == "inactive" {
        return Err(super::invalid(
            "document_conversion_conflict",
            document_id,
            "no previously deleted document at the conversion identity",
        ));
    }
    decode(&element)
}

fn decode(element: &Value) -> Result<Option<KnowledgeDocument>, PluginError> {
    let mut record = element["metadata"]["indexer_metadata"]["knowledge_document"].clone();
    if record.is_null() {
        return Ok(None);
    }
    if !record.is_object() || element["metadata"]["provider_instance_id"] != PROVIDER {
        return Err(super::invalid(
            "invalid_document_record",
            &record.to_string(),
            "document object owned by the Knowledge documents provider",
        ));
    }
    // Archive imports and remote adapters own root/ID remapping. Do not keep a second copy.
    record["semantic_element_id"] = element["semantic_element_id"].clone();
    record["project_root"] = element["project_root"].clone();
    let document: KnowledgeDocument = crate::parse(record, "stored Knowledge document")?;
    if element["path"] != storage_path(&document.document_id) {
        return Err(super::invalid(
            "invalid_document_record",
            &document.document_id,
            "matching indexed document storage path",
        ));
    }
    Ok(Some(document))
}

fn storage_path(document_id: &str) -> String {
    format!("@documents/{}", hex::encode(document_id))
}

pub(super) fn write(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let assets = super::assets::for_update(context, document)?;
    publish_document(context, document, assets.into_iter().collect())
}

pub(super) fn create(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let assets = super::assets::new_resource(context, document)?;
    publish_document(context, document, assets.into_iter().collect())
}

fn publish_document(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    artifacts: Vec<Value>,
) -> Result<(), PluginError> {
    let source = source_id(&document.project_root);
    let mut payload = serde_json::to_value(document).expect("serializable document");
    let fields = payload.as_object_mut().expect("document object");
    fields.remove("project_root");
    fields.remove("semantic_element_id");
    let element = json!({"semantic_element_id": document.semantic_element_id,
        "semantic_source_id": source, "semantic_element_type": "virtual_document",
        "semantic_element_name": document.title, "path": storage_path(&document.document_id),
        "parent_element_id": null, "start_line": null, "end_line": null,
        "content_fingerprint": null,
        "metadata": {"knowledge_document": payload}});
    publish(
        context,
        &document.project_root,
        vec![element],
        vec![],
        vec![storage_path(&document.document_id)],
        artifacts,
    )
}

fn source_id(root: &str) -> String {
    format!("knowledge-documents:{}", hex::encode(root))
}

fn publish(
    context: &mut PluginContext<'_>,
    root: &str,
    elements: Vec<Value>,
    removed: Vec<String>,
    replace: Vec<String>,
    artifacts: Vec<Value>,
) -> Result<(), PluginError> {
    crate::semantic::invoke_public(
        context,
        "ingest_index_batch",
        json!({
            "provider_instance_id": PROVIDER, "project_root": root,
            "replace_paths": replace, "removed_paths": removed,
            "semantic_sources": [{"semantic_source_id": source_id(root), "kind": "virtual_documents",
                "name": "Knowledge documents", "root_path": null, "root_uri": format!("lumvise:documents/{}", hex::encode(root))}],
            "semantic_elements": elements, "semantic_relationships": [], "semantic_artifacts": artifacts
        }),
    )?;
    Ok(())
}

pub(super) fn remove(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<(), PluginError> {
    publish(
        context,
        &identity.project_root,
        vec![],
        vec![storage_path(&identity.document_id)],
        vec![],
        vec![],
    )
}

pub(super) fn list(
    context: &mut PluginContext<'_>,
    request: &ListKnowledgeDocumentsRequest,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    let mut documents = Vec::new();
    for root in project_roots(context, request.project_root.as_deref())? {
        documents.extend(
            project_documents(context, &root)?
                .into_iter()
                .filter(|document| matches(document, request)),
        );
    }
    documents.sort_by(|left, right| {
        left.title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then(left.document_id.cmp(&right.document_id))
    });
    Ok(documents)
}

fn project_roots(
    context: &mut PluginContext<'_>,
    root: Option<&str>,
) -> Result<Vec<String>, PluginError> {
    if let Some(root) = root {
        crate::artifact::require(root, "project_root")?;
        return Ok(vec![root.into()]);
    }
    let response = context.host_call("storage.semantic", json!({"operation": "project_roots"}))?;
    let roots: Vec<String> =
        crate::parse(response["project_roots"].clone(), "project roots array")?;
    Ok(roots
        .into_iter()
        .filter(|root| !root.starts_with("agent-space://"))
        .collect())
}

fn project_documents(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    let response = context.host_call(
        "storage.semantic",
        json!({"operation": "scoped_read", "request": {
            "Structure": {"scope": {"project_root": root}, "max_depth": 0, "include_inactive": false}
        }}),
    )?;
    let elements = response["elements"].as_array().ok_or_else(|| {
        super::invalid(
            "invalid_document_storage",
            &response.to_string(),
            "elements array",
        )
    })?;
    elements
        .iter()
        .filter(|element| element["project_root"] == root)
        .filter_map(|element| decode(element).transpose())
        .collect()
}

pub(super) fn matches(
    document: &KnowledgeDocument,
    request: &ListKnowledgeDocumentsRequest,
) -> bool {
    if request
        .document_kind
        .is_some_and(|kind| kind != document.document_kind)
    {
        return false;
    }
    if !request.include_inactive && !crate::lifecycle::is_active(&document.lifecycle, Utc::now()) {
        return false;
    }
    let Some(query) = request
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
    else {
        return true;
    };
    let haystack = format!(
        "{} {} {} {}",
        document.title,
        document.content,
        document.path.as_deref().unwrap_or_default(),
        document.tags.join(" ")
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| haystack.contains(word))
}
