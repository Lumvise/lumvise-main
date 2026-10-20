//! A document owns only its outgoing citations; unresolved references stay in its payload.

use std::collections::{BTreeSet, HashSet};

use lumvise_contracts::{KnowledgeDocument, KnowledgeDocumentReference};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(super) fn relationships(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<Vec<Value>, PluginError> {
    let mut targets = semantic_targets(document);
    resolve_documents(context, document, &mut targets)?;
    let elements = crate::storage::elements_by_ids(
        context,
        &document.project_root,
        &targets.iter().cloned().collect(),
    )?;
    Ok(elements
        .into_iter()
        .filter(|element| {
            element.lifecycle == "active" && element.project_root == document.project_root
        })
        .map(|element| reference_edge(document, &element.semantic_element_id))
        .collect())
}

fn reference_edge(document: &KnowledgeDocument, target_id: &str) -> Value {
    json!({"source_element_id":document.semantic_element_id,"target_element_id":target_id,
        "relationship_kind":"references","relationship_label":"references"})
}

fn semantic_targets(document: &KnowledgeDocument) -> BTreeSet<String> {
    document
        .references
        .iter()
        .filter_map(|reference| match reference {
            KnowledgeDocumentReference::SemanticElement {
                semantic_element_id,
            } => Some(semantic_element_id.clone()),
            _ => None,
        })
        .collect()
}

fn resolve_documents(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    targets: &mut BTreeSet<String>,
) -> Result<(), PluginError> {
    for element in crate::storage::elements_by_ids(
        context,
        &document.project_root,
        &document_targets(document),
    )? {
        let record = json!(element);
        if element.lifecycle == "active" && super::record_format::locator(&record)?.is_some() {
            targets.insert(element.semantic_element_id);
        }
    }
    Ok(())
}

fn document_targets(document: &KnowledgeDocument) -> HashSet<String> {
    document
        .references
        .iter()
        .filter_map(|reference| match reference {
            KnowledgeDocumentReference::Document { document_id } => {
                Some(super::records::element_id(document_id))
            }
            _ => None,
        })
        .collect()
}
