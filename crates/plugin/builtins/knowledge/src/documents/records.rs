//! Single graph representation for Knowledge-owned independent documents.

use std::collections::HashSet;

use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeDocument, KnowledgeDocumentIdentity,
    KnowledgeDocumentReference, ListKnowledgeDocumentsRequest,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use super::record_format as format;

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
        KnowledgeDocumentReference::Qualified(citation) => citation.target.identity().1,
        KnowledgeDocumentReference::SemanticElement {
            semantic_element_id,
        } => semantic_element_id,
        KnowledgeDocumentReference::Artifact { artifact_id, .. } => artifact_id,
        KnowledgeDocumentReference::Document { document_id } => document_id,
    }
}

pub(super) fn find(
    context: &mut PluginContext<'_>,
    root: &str,
    document_id: &str,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    find_scoped(
        context,
        &KnowledgeDocumentIdentity {
            project_root: root.into(),
            document_id: document_id.into(),
        },
    )
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

pub(super) enum DocumentState {
    Missing,
    Stored(KnowledgeDocument),
    Deleted(KnowledgeDocument),
}

fn find_scoped(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    match scoped_state(context, identity)? {
        DocumentState::Stored(document) => Ok(Some(document)),
        DocumentState::Missing | DocumentState::Deleted(_) => Ok(None),
    }
}

pub(super) fn scoped_state(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<DocumentState, PluginError> {
    super::spaces::require_root(context, &identity.project_root)?;
    let Some(element) = selected_locator(context, &identity.project_root, &identity.document_id)?
    else {
        return Ok(DocumentState::Missing);
    };
    super::spaces::require_element(context, &element)?;
    if let Some(document) = format::deleted(&element)? {
        return Ok(DocumentState::Deleted(document));
    }
    let inactive = element["lifecycle"] == "inactive";
    let document = exact_document(context, element)?;
    Ok(if inactive {
        DocumentState::Deleted(document)
    } else {
        DocumentState::Stored(document)
    })
}

pub(super) fn selected_locator(
    context: &mut PluginContext<'_>,
    root: &str,
    id: &str,
) -> Result<Option<Value>, PluginError> {
    let rows = scoped_elements(
        context,
        root,
        json!({"Location":{
        "project_root":root, "path":format::storage_path(id), "line":1, "include_inactive":true}}),
    )?;
    match rows.as_slice() {
        [] => Ok(None),
        [element] if format::deleted(element)?.is_some() || format::locator(element)?.is_some() => {
            Ok(Some(element.clone()))
        }
        _ => Err(super::invalid(
            "document_identity_conflict",
            &json!(rows).to_string(),
            "one selected Knowledge document at its project path",
        )),
    }
}

pub(super) fn exact_document(
    context: &mut PluginContext<'_>,
    element: Value,
) -> Result<KnowledgeDocument, PluginError> {
    super::spaces::require_element(context, &element)?;
    if let Some(document) = format::deleted(&element)? {
        return Ok(document);
    }
    let locator = format::locator(&element)?.ok_or_else(|| {
        super::invalid(
            "invalid_document_record",
            &element.to_string(),
            "a document locator",
        )
    })?;
    let root = element["project_root"].as_str().ok_or_else(|| {
        super::invalid(
            "invalid_document_record",
            &element.to_string(),
            "a project-scoped locator",
        )
    })?;
    let body = exact_payload(context, root, &locator)?;
    let owner = body["semantic_element_id"].as_str().unwrap_or_default();
    let anchor = crate::storage::read_revision(
        context,
        root,
        lumvise_contracts::SemanticRecordRef::element(owner),
    )?;
    let roots = anchor
        .into_iter()
        .filter_map(|revision| match revision.body {
            lumvise_semantic_revisions::SemanticRevisionBody::Element(element) => {
                Some(json!(element))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    format::decode(&element, &body, &roots)
}

fn exact_payload(
    context: &mut PluginContext<'_>,
    root: &str,
    locator: &Value,
) -> Result<Value, PluginError> {
    let id = locator["artifact_id"].as_str().unwrap_or_default();
    let revision = crate::storage::read_revision(
        context,
        root,
        lumvise_contracts::SemanticRecordRef::artifact(id),
    )?;
    match revision.map(|revision| revision.body) {
        Some(lumvise_semantic_revisions::SemanticRevisionBody::Artifact(body)) => Ok(json!(body)),
        _ => Err(super::invalid(
            "document_revision_unavailable",
            id,
            "the exact stored document payload; retained IDs may have collected bodies",
        )),
    }
}

pub(super) fn is_document_record(metadata: &Value) -> bool {
    format::is_owned(metadata)
}

pub(super) fn decode(
    element: &Value,
    artifacts: &[Value],
    roots: &[Value],
) -> Result<Option<KnowledgeDocument>, PluginError> {
    if let Some(document) = format::deleted(element)? {
        return Ok(Some(document));
    }
    let Some(locator) = format::locator(element)? else {
        return Ok(None);
    };
    let artifact = artifacts
        .iter()
        .find(|row| row["artifact_id"] == locator["artifact_id"])
        .unwrap_or(&Value::Null);
    format::decode(element, artifact, roots).map(Some)
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
        .filter(|root| !super::spaces::is_private_root(root))
        .collect())
}

fn project_documents(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    project_document_records(context, root, false)
}

pub(super) fn project_document_records(
    context: &mut PluginContext<'_>,
    root: &str,
    include_deleted: bool,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    let mut elements = project_elements(context, root, include_deleted)?;
    super::spaces::filter_elements(context, root, &mut elements)?;
    let artifacts = read_indexed_bodies(context, &elements)?;
    elements
        .iter()
        .filter(|element| element["project_root"] == root)
        .filter(|element| {
            include_deleted
                || (element["lifecycle"] != "inactive"
                    && format::deleted(element).is_ok_and(|deleted| deleted.is_none()))
        })
        .filter_map(|element| decode(element, &artifacts, &elements).transpose())
        .collect()
}

pub(super) fn project_elements(
    context: &mut PluginContext<'_>,
    root: &str,
    include_deleted: bool,
) -> Result<Vec<Value>, PluginError> {
    Ok(scoped_elements(
        context,
        root,
        json!({"PathPrefix": {"project_root": root, "prefix": format::PATH_PREFIX,
        "include_inactive": include_deleted}}),
    )?
    .into_iter()
    .filter(|element| element["project_root"] == root)
    .collect())
}

pub(super) fn scoped_elements(
    context: &mut PluginContext<'_>,
    root: &str,
    request: Value,
) -> Result<Vec<Value>, PluginError> {
    super::spaces::require_root(context, root)?;
    let response = context.host_call(
        "storage.semantic",
        json!({"operation":"scoped_read","request":request}),
    )?;
    if response["project_root"] != root {
        return Err(super::invalid(
            "invalid_document_storage",
            &response["project_root"].to_string(),
            &format!("scoped elements for project {root}"),
        ));
    }
    response_array(&response, "elements")
}

pub(super) fn captured_records(
    context: &mut PluginContext<'_>,
    root: &str,
    mut elements: Vec<crate::semantic_context::SemanticElement>,
) -> Result<(Vec<crate::semantic_context::SemanticElement>, Vec<Value>), PluginError> {
    for element in &elements {
        validate_element_scope(root, element)?;
    }
    let records: Vec<Value> = elements.iter().map(|element| json!(element)).collect();
    let artifacts = read_indexed_bodies(context, &records)?;
    let roots = artifacts
        .iter()
        .filter_map(|artifact| artifact["semantic_element_id"].as_str().map(str::to_owned))
        .collect();
    elements.extend(crate::storage::elements_by_ids(context, root, &roots)?);
    Ok((elements, artifacts))
}

fn read_indexed_bodies(
    context: &mut PluginContext<'_>,
    elements: &[Value],
) -> Result<Vec<Value>, PluginError> {
    let mut artifact_ids = HashSet::new();
    for element in elements {
        if let Some(locator) = format::locator(element)? {
            artifact_ids.insert(crate::required_input(locator, "artifact_id")?);
        }
    }
    if artifact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let response = context.host_call(
        "storage.semantic",
        json!({"operation":"artifacts_by_ids", "artifact_ids":artifact_ids}),
    )?;
    response_array(&response, "artifacts")
}

pub(super) fn validate_element_scope(
    root: &str,
    element: &crate::semantic_context::SemanticElement,
) -> Result<(), PluginError> {
    if element.project_root == root {
        return Ok(());
    }
    Err(super::invalid(
        "indexed_document_scope_mismatch",
        &format!(
            "{} in {}",
            element.semantic_element_id, element.project_root
        ),
        &format!("an element in project {root}"),
    ))
}

fn response_array(response: &Value, field: &str) -> Result<Vec<Value>, PluginError> {
    response[field].as_array().cloned().ok_or_else(|| {
        super::invalid(
            "invalid_document_storage",
            &response.to_string(),
            &format!("{field} array"),
        )
    })
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
