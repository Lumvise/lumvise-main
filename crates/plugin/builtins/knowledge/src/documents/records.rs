//! Single graph representation for Knowledge-owned independent documents.

use std::collections::HashSet;

use chrono::Utc;
use lumvise_contracts::{
    CreateKnowledgeDocumentRequest, KnowledgeDocument, KnowledgeDocumentIdentity,
    KnowledgeDocumentReference, ListKnowledgeDocumentsRequest,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use super::{publication, record_format as format};

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
    if element
        .as_ref()
        .is_some_and(|row| !is_document_record(&row["metadata"]))
    {
        return Err(super::invalid(
            "document_identity_conflict",
            document_id,
            "an unused index identity or a Knowledge-owned document",
        ));
    }
    match element {
        Some(element) if element["lifecycle"] != "inactive" => {
            find_scoped(context, &indexed_identity(&element, document_id)?)
        }
        _ => Ok(None),
    }
}

fn indexed_identity(
    element: &Value,
    document_id: &str,
) -> Result<KnowledgeDocumentIdentity, PluginError> {
    let root = element["project_root"].as_str().ok_or_else(|| {
        super::invalid(
            "invalid_document_record",
            &element.to_string(),
            "project_root string",
        )
    })?;
    Ok(KnowledgeDocumentIdentity {
        project_root: root.into(),
        document_id: document_id.into(),
    })
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
    Deleted,
}

fn find_scoped(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    match scoped_state(context, identity)? {
        DocumentState::Stored(document) => Ok(Some(document)),
        DocumentState::Missing | DocumentState::Deleted => Ok(None),
    }
}

pub(super) fn scoped_state(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<DocumentState, PluginError> {
    let elements = crate::storage::elements_by_ids(
        context,
        &identity.project_root,
        &HashSet::from([element_id(&identity.document_id)]),
    )?;
    let (elements, artifacts) = captured_records(context, &identity.project_root, elements)?;
    let roots: Vec<Value> = elements.iter().map(|row| json!(row)).collect();
    for element in roots.iter() {
        if let Some(document) = decode(element, &artifacts, &roots)?
            && document.document_id == identity.document_id
        {
            return Ok(if element["lifecycle"] == "inactive" {
                DocumentState::Deleted
            } else {
                DocumentState::Stored(document)
            });
        }
    }
    Ok(DocumentState::Missing)
}

pub(super) fn find_for_conversion(
    context: &mut PluginContext<'_>,
    root: &str,
    document_id: &str,
) -> Result<Option<KnowledgeDocument>, PluginError> {
    match scoped_state(
        context,
        &KnowledgeDocumentIdentity {
            project_root: root.into(),
            document_id: document_id.into(),
        },
    )? {
        DocumentState::Stored(document) => Ok(Some(document)),
        DocumentState::Missing => find(context, document_id),
        DocumentState::Deleted => Err(super::invalid(
            "document_conversion_conflict",
            document_id,
            "no previously deleted document at the conversion identity",
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
    let Some(locator) = format::locator(element)? else {
        return Ok(None);
    };
    let artifact = artifacts
        .iter()
        .find(|row| row["artifact_id"] == locator["artifact_id"])
        .unwrap_or(&Value::Null);
    format::decode(element, artifact, roots).map(Some)
}

pub(super) fn write(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let assets = super::assets::for_update(context, document)?;
    publication::write(context, document, assets.into_iter().collect())
}

pub(super) fn create(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<(), PluginError> {
    let assets = super::assets::new_resource(context, document)?;
    publication::write(context, document, assets.into_iter().collect())
}

pub(super) fn remove(
    context: &mut PluginContext<'_>,
    identity: &KnowledgeDocumentIdentity,
) -> Result<(), PluginError> {
    publication::remove(context, identity)
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
    project_document_records(context, root, false)
}

pub(super) fn project_document_records(
    context: &mut PluginContext<'_>,
    root: &str,
    include_deleted: bool,
) -> Result<Vec<KnowledgeDocument>, PluginError> {
    let elements = project_elements(context, root, include_deleted)?;
    let artifacts = read_indexed_bodies(context, &elements)?;
    elements
        .iter()
        .filter(|element| element["project_root"] == root)
        .filter(|element| include_deleted || element["lifecycle"] != "inactive")
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
        json!({"Structure": {"scope": {"project_root": root},
        "max_depth":0,"include_inactive":include_deleted}}),
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
    let mut artifacts = Vec::new();
    for element in elements {
        if let Some(locator) = format::locator(element)? {
            let response = context.host_call(
                "storage.semantic",
                json!({"operation":"artifact",
                "artifact_id":locator["artifact_id"]}),
            )?;
            artifacts.push(response["artifact"].clone());
        }
    }
    Ok(artifacts)
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
