use super::*;

#[cfg(test)]
pub(crate) fn insert_relationship_node(
    database: &GraphTransaction<'_>,
    relationship: &SemanticRelationship,
) -> Result<()> {
    let node_ids = semantic_node_id_index(database, &relationship.project_root);
    insert_relationship_edge_with_index(database, relationship, &node_ids)
}
#[cfg(test)]
pub(crate) fn insert_relationship_edge_with_index(
    database: &GraphTransaction<'_>,
    relationship: &SemanticRelationship,
    node_ids: &BTreeMap<String, NodeId>,
) -> Result<()> {
    let Some(source_id) = node_ids.get(&relationship.source_element_id).copied() else {
        return Ok(());
    };
    let Some(target_id) = node_ids.get(&relationship.target_element_id).copied() else {
        return Ok(());
    };
    database.create_edge_with_props(
        source_id,
        target_id,
        &relationship.relationship_kind,
        [
            (
                PROJECT_ROOT_PROPERTY,
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
    )?;
    Ok(())
}
/// Collects commit-coalesced dirty elements for hook delivery.
pub(crate) struct ChangeCollector {
    commit_version: i64,
    dirty_elements: BTreeMap<String, DirtyElement>,
}

impl ChangeCollector {
    pub(crate) fn new(commit_version: i64) -> Self {
        Self {
            commit_version,
            dirty_elements: BTreeMap::new(),
        }
    }

    pub(crate) fn record_element(
        &mut self,
        element: &SemanticElement,
        disposition: ChangeDisposition,
    ) {
        self.dirty_elements.insert(
            element.semantic_element_id.clone(),
            DirtyElement {
                element_id: element.semantic_element_id.clone(),
                project_root: element.project_root.clone(),
                entity_kind: element.element_kind.clone(),
                revision: self.commit_version,
                disposition,
            },
        );
    }

    pub(crate) fn record_removal(
        &mut self,
        element_id: &str,
        project_root: &str,
        entity_kind: &str,
    ) {
        self.dirty_elements.insert(
            element_id.to_string(),
            DirtyElement {
                element_id: element_id.to_string(),
                project_root: project_root.to_string(),
                entity_kind: entity_kind.to_string(),
                revision: self.commit_version,
                disposition: ChangeDisposition::Removal,
            },
        );
    }

    pub(crate) fn into_parts(self) -> Vec<DirtyElement> {
        self.dirty_elements.into_values().collect()
    }
}

pub(crate) fn semantic_relationships_from_native_edges(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Vec<SemanticRelationship> {
    let Some(source_id) = semantic_element_node_id(database, semantic_element_id) else {
        return Vec::new();
    };
    database
        .store()
        .edges_from(source_id, Direction::Outgoing)
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter_map(|edge| semantic_relationship_from_edge(&edge))
        .collect()
}

pub(crate) fn semantic_relationships_from_many_native_edges(
    database: &GrafeoDB,
    semantic_element_ids: &std::collections::BTreeSet<String>,
) -> Vec<SemanticRelationship> {
    semantic_element_ids
        .iter()
        .flat_map(|element_id| semantic_relationships_from_native_edges(database, element_id))
        .collect()
}

pub(crate) fn semantic_relationships_touching_elements(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Vec<SemanticRelationship> {
    let nodes = selected_relationship_nodes(database, semantic_element_ids);
    let edges = nodes
        .into_iter()
        .flat_map(|node| database.graph_store().edges_from(node, Direction::Both))
        .map(|(_, edge)| edge)
        .collect::<HashSet<_>>();
    let mut relationships = BTreeMap::new();
    for relationship in selective_relationships(database, &edges.into_iter().collect::<Vec<_>>()) {
        insert_relationship(&mut relationships, Some(relationship));
    }
    relationships.into_values().collect()
}

fn selected_relationship_nodes(database: &GrafeoDB, ids: &HashSet<String>) -> Vec<NodeId> {
    let nodes = active_element_nodes(database, ids);
    let identities = database
        .graph_store()
        .get_node_property_batch(&nodes, &SEMANTIC_ELEMENT_ID_PROPERTY.into());
    let mut selected = BTreeMap::new();
    for (node, identity) in nodes.into_iter().zip(identities) {
        let Some(id) = identity.as_ref().and_then(value_string) else {
            continue;
        };
        let entry = selected.entry(id.clone()).or_insert(node);
        // Preserve the single-record lookup's preference when repairing duplicate identities.
        if node == semantic_element_node_id_for(&id) {
            *entry = node;
        }
    }
    selected.into_values().collect()
}

pub(super) fn insert_native_relationships(
    database: &GrafeoDB,
    semantic_element_id: &str,
    relationships: &mut BTreeMap<(String, String, String, String), SemanticRelationship>,
) {
    let Some(node_id) = semantic_element_node_id(database, semantic_element_id) else {
        return;
    };
    for (_, edge_id) in database.store().edges_from(node_id, Direction::Both) {
        let relationship = database
            .get_edge(edge_id)
            .and_then(|edge| semantic_relationship_from_edge(&edge));
        insert_relationship(relationships, relationship);
    }
}

pub(super) fn insert_relationship(
    relationships: &mut BTreeMap<(String, String, String, String), SemanticRelationship>,
    relationship: Option<SemanticRelationship>,
) {
    let Some(relationship) = relationship else {
        return;
    };
    let key = (
        relationship.source_element_id.clone(),
        relationship.target_element_id.clone(),
        relationship.relationship_kind.clone(),
        relationship.label.clone(),
    );
    relationships.insert(key, relationship);
}

pub(crate) fn semantic_relationships_for_project_edges(
    database: &GrafeoDB,
    project_root: &str,
) -> Vec<SemanticRelationship> {
    let nodes = node_ids_by_label_and_property(
        database,
        "SemanticElement",
        PROJECT_ROOT_PROPERTY,
        project_root,
    );
    selective_adjacency(database, &nodes, project_root).0
}
