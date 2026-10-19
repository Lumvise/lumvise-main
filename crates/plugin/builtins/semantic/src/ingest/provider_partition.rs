//! A full provider refresh must not erase another provider's graph records.

use std::collections::{BTreeMap, HashSet};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::{
    models::{IndexBatchRequest, SemanticElement, SemanticRelationship},
    storage,
};

pub(super) fn preserve_foreign(
    context: &mut PluginContext<'_>,
    batch: &IndexBatchRequest,
    incoming: &BTreeMap<String, SemanticElement>,
    relationships: &BTreeMap<String, SemanticRelationship>,
) -> Result<(Vec<SemanticElement>, Vec<SemanticRelationship>), PluginError> {
    let mut elements = incoming.values().cloned().collect::<Vec<_>>();
    let mut edges = relationships.values().cloned().collect::<Vec<_>>();
    if !storage::graph_project_roots(context)?.contains(&batch.project_root) {
        return Ok((elements, edges));
    }
    let snapshot = storage::project_snapshot::<SemanticElement, SemanticRelationship, Value>(
        context,
        &json!({"project_root": batch.project_root}),
        None,
    )?;
    let foreign = foreign_elements(snapshot.elements, &batch.provider_instance_id, incoming);
    preserve_edges(&foreign, &elements, snapshot.relationships, &mut edges);
    elements.extend(foreign);
    Ok((elements, edges))
}

fn foreign_elements(
    existing: Vec<SemanticElement>,
    provider: &str,
    incoming: &BTreeMap<String, SemanticElement>,
) -> Vec<SemanticElement> {
    existing
        .into_iter()
        .filter(|element| {
            element.lifecycle != "inactive"
                && !incoming.contains_key(&element.semantic_element_id)
                && element.metadata["provider_instance_id"]
                    .as_str()
                    .is_some_and(|owner| owner != provider)
        })
        .collect()
}

fn preserve_edges(
    foreign: &[SemanticElement],
    incoming: &[SemanticElement],
    existing: Vec<SemanticRelationship>,
    edges: &mut Vec<SemanticRelationship>,
) {
    let foreign_ids = foreign
        .iter()
        .map(|element| &element.semantic_element_id)
        .collect::<HashSet<_>>();
    let active_ids = incoming
        .iter()
        .chain(foreign)
        .map(|element| &element.semantic_element_id)
        .collect::<HashSet<_>>();
    for edge in existing {
        if (foreign_ids.contains(&edge.source_element_id)
            || foreign_ids.contains(&edge.target_element_id))
            && active_ids.contains(&edge.source_element_id)
            && active_ids.contains(&edge.target_element_id)
            && !edges.iter().any(|candidate| {
                candidate.source_element_id == edge.source_element_id
                    && candidate.target_element_id == edge.target_element_id
                    && candidate.relationship_kind == edge.relationship_kind
            })
        {
            edges.push(edge);
        }
    }
}
