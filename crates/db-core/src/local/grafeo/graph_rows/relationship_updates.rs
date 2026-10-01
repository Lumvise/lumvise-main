use super::{semantic_element_node_id, semantic_relationship_from_edge};
use crate::local::grafeo::graph_store::GraphTransaction;
use crate::{Result, SemanticRelationship};
use grafeo::{EdgeId, GrafeoDB, NodeId, Value as GrafeoValue};
use grafeo_core::graph::Direction;
use std::collections::{BTreeMap, BTreeSet};

type RelationshipKey = (String, String, String, String);

pub(crate) struct RelationshipWritePlan {
    delete_edge_ids: Vec<EdgeId>,
    updates: Vec<RelationshipEdgeUpdate>,
    inserts: Vec<RelationshipEdgeInsert>,
    changed_element_node_ids: BTreeMap<String, NodeId>,
}
struct RelationshipEdgeUpdate {
    edge_id: EdgeId,
    properties: Vec<(&'static str, GrafeoValue)>,
}

struct RelationshipEdgeInsert {
    source_element_id: String,
    target_element_id: String,
    source_node_id: NodeId,
    target_node_id: NodeId,
    relationship_kind: String,
    properties: Vec<(&'static str, GrafeoValue)>,
}
/// plan only mutates the prepared edge and node IDs.
pub(crate) fn prepare_relationship_sync(
    database: &GrafeoDB,
    source_element_ids: &BTreeSet<String>,
    relationships: &[SemanticRelationship],
    known_node_ids: &BTreeMap<String, NodeId>,
) -> Result<RelationshipWritePlan> {
    let node_ids = relationship_node_ids(
        database,
        source_element_ids
            .iter()
            .map(String::as_str)
            .chain(relationships.iter().flat_map(|relationship| {
                [
                    relationship.source_element_id.as_str(),
                    relationship.target_element_id.as_str(),
                ]
            })),
        known_node_ids,
    );
    let (existing, redundant_edges) =
        existing_relationship_edges(database, source_element_ids, &node_ids);
    let desired = relationships
        .iter()
        .map(|relationship| (relationship_key(relationship), relationship))
        .collect::<BTreeMap<_, _>>();
    let mut delete_edge_ids = existing
        .iter()
        .filter(|(key, _)| !desired.contains_key(*key))
        .map(|(_, (edge_id, _))| *edge_id)
        .collect::<Vec<_>>();
    delete_edge_ids.extend(redundant_edges);
    let mut changed_element_ids = BTreeSet::new();
    for (key, (_, current)) in existing.iter() {
        if !desired.contains_key(key) {
            changed_element_ids.insert(current.source_element_id.clone());
            changed_element_ids.insert(current.target_element_id.clone());
        }
    }
    let mut updates = Vec::new();
    let mut inserts = Vec::new();
    for (key, relationship) in desired {
        match existing.get(&key) {
            Some((edge_id, current)) if current.metadata != relationship.metadata => {
                changed_element_ids.insert(relationship.source_element_id.clone());
                changed_element_ids.insert(relationship.target_element_id.clone());
                updates.push(RelationshipEdgeUpdate {
                    edge_id: *edge_id,
                    properties: vec![(
                        "metadata_json",
                        GrafeoValue::from(serde_json::to_string(&relationship.metadata)?),
                    )],
                });
            }
            Some(_) => {}
            None => {
                if let (Some(source_node_id), Some(target_node_id)) = (
                    node_ids.get(&relationship.source_element_id),
                    node_ids.get(&relationship.target_element_id),
                ) {
                    changed_element_ids.insert(relationship.source_element_id.clone());
                    changed_element_ids.insert(relationship.target_element_id.clone());
                    inserts.push(relationship_edge_insert(
                        *source_node_id,
                        *target_node_id,
                        relationship,
                    )?);
                }
            }
        }
    }
    let changed_element_node_ids = changed_element_ids
        .iter()
        .filter_map(|id| node_ids.get(id).map(|node_id| (id.clone(), *node_id)))
        .collect();
    Ok(RelationshipWritePlan {
        delete_edge_ids,
        updates,
        inserts,
        changed_element_node_ids,
    })
}

/// Plans independent relationship upserts without removing unrelated outgoing
/// relationships. This preserves append/link semantics.
pub(crate) fn prepare_relationship_upserts(
    database: &GrafeoDB,
    relationships: &[SemanticRelationship],
    known_node_ids: &BTreeMap<String, NodeId>,
) -> Result<RelationshipWritePlan> {
    let source_ids = relationships
        .iter()
        .map(|relationship| relationship.source_element_id.clone())
        .collect::<BTreeSet<_>>();
    let node_ids = relationship_node_ids(
        database,
        relationships.iter().flat_map(|relationship| {
            [
                relationship.source_element_id.as_str(),
                relationship.target_element_id.as_str(),
            ]
        }),
        known_node_ids,
    );
    let (existing, redundant_edges) = existing_relationship_edges(database, &source_ids, &node_ids);
    let desired = relationships
        .iter()
        .map(|relationship| (relationship_key(relationship), relationship))
        .collect::<BTreeMap<_, _>>();
    let mut delete_edge_ids = existing
        .iter()
        .filter(|(key, _)| desired.contains_key(*key))
        .map(|(_, (edge_id, _))| *edge_id)
        .collect::<Vec<_>>();
    delete_edge_ids.extend(redundant_edges);
    let mut changed_element_ids = BTreeSet::new();
    let mut inserts = Vec::new();
    for relationship in desired.into_values() {
        if let (Some(source_node_id), Some(target_node_id)) = (
            node_ids.get(&relationship.source_element_id),
            node_ids.get(&relationship.target_element_id),
        ) {
            changed_element_ids.insert(relationship.source_element_id.clone());
            changed_element_ids.insert(relationship.target_element_id.clone());
            inserts.push(relationship_edge_insert(
                *source_node_id,
                *target_node_id,
                relationship,
            )?);
        }
    }
    let changed_element_node_ids = changed_element_ids
        .iter()
        .filter_map(|id| node_ids.get(id).map(|node_id| (id.clone(), *node_id)))
        .collect();
    Ok(RelationshipWritePlan {
        delete_edge_ids,
        updates: Vec::new(),
        inserts,
        changed_element_node_ids,
    })
}

impl RelationshipWritePlan {
    pub(crate) fn changed_element_node_ids(&self) -> &BTreeMap<String, NodeId> {
        &self.changed_element_node_ids
    }
}
pub(crate) fn apply_relationship_write(
    database: &GraphTransaction<'_>,
    plan: RelationshipWritePlan,
) -> Result<()> {
    for edge_id in plan.delete_edge_ids {
        database.delete_edge(edge_id);
    }
    for update in plan.updates {
        for (property, value) in update.properties {
            database.set_edge_property(update.edge_id, property, value)?;
        }
    }
    for insert in plan.inserts {
        let source_node_id = database
            .semantic_node_id(&insert.source_element_id)
            .unwrap_or(insert.source_node_id);
        let target_node_id = database
            .semantic_node_id(&insert.target_element_id)
            .unwrap_or(insert.target_node_id);
        database.create_edge_with_props(
            source_node_id,
            target_node_id,
            &insert.relationship_kind,
            insert.properties,
        )?;
    }
    Ok(())
}

fn relationship_node_ids<'a>(
    database: &GrafeoDB,
    element_ids: impl Iterator<Item = &'a str>,
    known_node_ids: &BTreeMap<String, NodeId>,
) -> BTreeMap<String, NodeId> {
    let mut node_ids = known_node_ids.clone();
    for element_id in element_ids.collect::<BTreeSet<_>>() {
        if node_ids.contains_key(element_id) {
            continue;
        }
        if let Some(node_id) = semantic_element_node_id(database, element_id) {
            node_ids.insert(element_id.to_string(), node_id);
        }
    }
    node_ids
}

fn existing_relationship_edges(
    database: &GrafeoDB,
    source_element_ids: &BTreeSet<String>,
    node_ids: &BTreeMap<String, NodeId>,
) -> (
    BTreeMap<RelationshipKey, (EdgeId, SemanticRelationship)>,
    Vec<EdgeId>,
) {
    let mut existing = BTreeMap::new();
    let mut redundant_edges = Vec::new();
    let mut seen_edge_ids = BTreeSet::new();
    for source_element_id in source_element_ids {
        let Some(source_node_id) = node_ids.get(source_element_id) else {
            continue;
        };
        let source_nodes = super::node_ids_by_label_and_property(
            database,
            "SemanticElement",
            "semantic_element_id",
            source_element_id,
        );
        for (_, edge_id) in source_nodes
            .iter()
            .chain(std::iter::once(source_node_id))
            .flat_map(|node_id| database.store().edges_from(*node_id, Direction::Outgoing))
        {
            if !seen_edge_ids.insert(edge_id) {
                continue;
            }
            let Some(edge) = database.get_edge(edge_id) else {
                continue;
            };
            if let Some(relationship) = semantic_relationship_from_edge(&edge) {
                if let Some((previous, _)) =
                    existing.insert(relationship_key(&relationship), (edge.id, relationship))
                {
                    redundant_edges.push(previous);
                }
            }
        }
    }
    (existing, redundant_edges)
}

fn relationship_edge_insert(
    source_node_id: NodeId,
    target_node_id: NodeId,
    relationship: &SemanticRelationship,
) -> Result<RelationshipEdgeInsert> {
    Ok(RelationshipEdgeInsert {
        source_element_id: relationship.source_element_id.clone(),
        target_element_id: relationship.target_element_id.clone(),
        source_node_id,
        target_node_id,
        relationship_kind: relationship.relationship_kind.clone(),
        properties: vec![
            (
                "project_root",
                GrafeoValue::from(relationship.project_root.clone()),
            ),
            (
                "source_element_id",
                GrafeoValue::from(relationship.source_element_id.clone()),
            ),
            (
                "target_element_id",
                GrafeoValue::from(relationship.target_element_id.clone()),
            ),
            (
                "relationship_kind",
                GrafeoValue::from(relationship.relationship_kind.clone()),
            ),
            ("label", GrafeoValue::from(relationship.label.clone())),
            (
                "metadata_json",
                GrafeoValue::from(serde_json::to_string(&relationship.metadata)?),
            ),
        ],
    })
}

fn relationship_key(relationship: &SemanticRelationship) -> RelationshipKey {
    (
        relationship.source_element_id.clone(),
        relationship.target_element_id.clone(),
        relationship.relationship_kind.clone(),
        relationship.label.clone(),
    )
}
