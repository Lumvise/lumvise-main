//! A document owns only its outgoing citations; unresolved references stay in its payload.

use std::collections::BTreeSet;

use lumvise_contracts::{KnowledgeDocument, KnowledgeDocumentReference};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

/// Existing citations become canonical dependencies; unresolved unversioned links remain drafts.
/// Shared publication compares the original edit counter and decides which bodies must freeze.
pub(super) fn artifact_dependencies(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
) -> Result<Vec<Value>, PluginError> {
    let mut dependencies = Vec::new();
    for reference in &document.references {
        let KnowledgeDocumentReference::Artifact {
            artifact_id,
            edit_version,
        } = reference
        else {
            continue;
        };
        if edit_version.is_none()
            && crate::storage::read_revision(
                context,
                &document.project_root,
                lumvise_contracts::SemanticRecordRef::artifact(artifact_id),
            )?
            .is_none()
        {
            continue;
        }
        dependencies.push(json!({"target":reference}));
    }
    Ok(dependencies)
}

pub(super) fn relationships(
    context: &mut PluginContext<'_>,
    document: &KnowledgeDocument,
    source_id: &str,
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
        .map(|element| reference_edge(source_id, &element.semantic_element_id))
        .collect())
}

fn reference_edge(source_id: &str, target_id: &str) -> Value {
    json!({"source_element_id":source_id,"target_element_id":target_id,
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
    for reference in &document.references {
        let KnowledgeDocumentReference::Document { document_id } = reference else {
            continue;
        };
        if let Some(target) = super::records::find(context, &document.project_root, document_id)? {
            targets.insert(target.semantic_element_id);
        }
    }
    Ok(())
}
