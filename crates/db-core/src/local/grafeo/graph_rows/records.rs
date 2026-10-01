use super::*;

#[cfg(test)]
pub(crate) fn semantic_node_id_index(
    database: &GraphTransaction<'_>,
    project_root: &str,
) -> BTreeMap<String, NodeId> {
    let mut node_ids = database
        .find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root))
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .filter(|node| node.has_label("SemanticElement"))
        .filter_map(|node| {
            string_property(&node, SEMANTIC_ELEMENT_ID_PROPERTY)
                .map(|semantic_id| (semantic_id, node.id))
        })
        .collect::<BTreeMap<_, _>>();
    node_ids.extend(database.semantic_node_ids());
    node_ids
}

pub(crate) struct CompleteProjectRows {
    pub(crate) elements: Vec<SemanticElement>,
    pub(crate) relationships: Vec<SemanticRelationship>,
    pub(crate) artifacts: Vec<SemanticArtifact>,
}

const COMPLETE_ELEMENT_PROPERTIES: &[&str] = &[
    "project_root",
    "semantic_element_id",
    "semantic_source_id",
    "path",
    "element_kind",
    "name",
    "parent_element_id",
    "content_fingerprint",
    "start_line",
    "end_line",
    "lifecycle",
    "match_confidence",
    "simhash_distance",
    "matched_at",
    "precaution",
    "metadata_json",
];

const COMPLETE_ARTIFACT_PROPERTIES: &[&str] = &[
    "artifact_id",
    "semantic_element_id",
    "artifact_kind",
    "title",
    "content_ref",
    "content",
    "searchable_text",
    "content_size_bytes",
    "dependencies_json",
    "metadata_json",
];
pub(crate) fn semantic_element_by_id_selective(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Option<SemanticElement> {
    let ids = [semantic_element_id.to_string()];
    let node_ids = ids
        .iter()
        .flat_map(|id| {
            database.find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from(id.as_str()),
            )
        })
        .collect::<Vec<_>>();
    selective_elements(database, &node_ids, None, false)
        .into_iter()
        .next()
}

const COMPLETE_EDGE_PROPERTIES: &[&str] = &[
    "project_root",
    "source_element_id",
    "target_element_id",
    "relationship_kind",
    "label",
    "artifact_id",
    "semantic_element_id",
    "metadata_json",
];

pub(crate) fn complete_project_rows(
    database: &GrafeoDB,
    project_root: &str,
) -> CompleteProjectRows {
    let element_ids =
        database.find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root));
    let elements = selective_elements(database, &element_ids, Some(project_root), true);
    let (relationships, artifact_ids) = selective_adjacency(database, &element_ids, project_root);
    let artifacts = selective_artifacts(database, &elements, &artifact_ids, None);
    CompleteProjectRows {
        elements,
        relationships,
        artifacts,
    }
}

pub(crate) fn project_artifacts_selective(
    database: &GrafeoDB,
    project_root: &str,
    artifact_namespace: Option<&str>,
) -> Vec<SemanticArtifact> {
    let node_ids =
        database.find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root));
    let owners = semantic_owner_ids(database, &node_ids);
    selective_artifacts_for_owners(database, owners, artifact_namespace)
}

pub(crate) fn artifacts_by_ids_selective(
    database: &GrafeoDB,
    artifact_ids: &HashSet<String>,
) -> Vec<SemanticArtifact> {
    let node_ids = artifact_ids
        .iter()
        .flat_map(|id| {
            database.find_nodes_by_property(ARTIFACT_ID_PROPERTY, &GrafeoValue::from(id.as_str()))
        })
        .collect::<Vec<_>>();
    selective_artifacts(database, &[], &node_ids, None)
}
pub(super) fn selective_node_property_batch(
    store: &dyn GrafeoGraphStore,
    node_ids: &[NodeId],
    key: &str,
) -> Vec<Option<GrafeoValue>> {
    #[cfg(test)]
    record_selective_node_batch_call();
    store.get_node_property_batch(node_ids, &key.into())
}

pub(crate) fn elements_by_ids_selective(
    database: &GrafeoDB,
    project_root: &str,
    semantic_element_ids: &HashSet<String>,
    include_inactive: bool,
) -> Vec<SemanticElement> {
    let node_ids = semantic_element_ids
        .iter()
        .flat_map(|id| {
            database.find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from(id.as_str()),
            )
        })
        .collect::<Vec<_>>();
    selective_elements(database, &node_ids, Some(project_root), include_inactive)
}

/// Unions equality-index hits across the three exact identity keys Knowledge's
/// cross-project inheritance admits candidates through - raw content
/// fingerprint, normalized `(kind, name)`, and normalized `(kind, file name)`
/// - into one deduplicated candidate element set spanning every project.
pub(crate) fn candidate_elements_by_identity_keys(
    database: &GrafeoDB,
    content_fingerprints: &HashSet<String>,
    kind_name_keys: &HashSet<String>,
    kind_file_name_keys: &HashSet<String>,
) -> Vec<SemanticElement> {
    let mut node_ids = HashSet::new();
    for value in content_fingerprints {
        node_ids.extend(database.find_nodes_by_property(
            CONTENT_FINGERPRINT_PROPERTY,
            &GrafeoValue::from(value.as_str()),
        ));
    }
    for value in kind_name_keys {
        node_ids.extend(database.find_nodes_by_property(
            IDENTITY_KIND_NAME_PROPERTY,
            &GrafeoValue::from(value.as_str()),
        ));
    }
    for value in kind_file_name_keys {
        node_ids.extend(database.find_nodes_by_property(
            IDENTITY_KIND_FILE_NAME_PROPERTY,
            &GrafeoValue::from(value.as_str()),
        ));
    }
    let node_ids = node_ids.into_iter().collect::<Vec<_>>();
    selective_elements(database, &node_ids, None, true)
}

pub(crate) fn selective_elements(
    database: &GrafeoDB,
    node_ids: &[NodeId],
    project_root: Option<&str>,
    include_inactive: bool,
) -> Vec<SemanticElement> {
    let store = database.graph_store();
    let keys = COMPLETE_ELEMENT_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_node_batch_call();
    let rows = store.get_nodes_properties_selective_batch(node_ids, &keys);
    node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, _)| {
            element_from_properties(|key| rows.get(row).and_then(|properties| properties.get(key)))
        })
        .filter(|element| project_root.is_none_or(|root| element.project_root == root))
        .filter(|element| include_inactive || element.lifecycle != "inactive")
        .collect()
}

pub(super) fn element_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticElement> {
    Some(SemanticElement {
        project_root: property("project_root").and_then(value_string)?,
        semantic_element_id: property("semantic_element_id").and_then(value_string)?,
        semantic_source_id: property("semantic_source_id").and_then(value_string)?,
        path: property("path").and_then(value_string)?,
        element_kind: property("element_kind").and_then(value_string)?,
        name: property("name").and_then(value_string)?,
        parent_element_id: property("parent_element_id").and_then(value_non_empty),
        content_fingerprint: property("content_fingerprint").and_then(value_non_empty),
        start_line: property("start_line").and_then(value_non_negative_i64),
        end_line: property("end_line").and_then(value_non_negative_i64),
        lifecycle: property("lifecycle")
            .and_then(value_string)
            .unwrap_or_else(|| "active".to_string()),
        match_evidence: match_evidence_from_properties(&property),
        metadata: property("metadata_json")
            .map(value_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn match_evidence_from_properties<'a>(
    property: &impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticMatchEvidence> {
    Some(SemanticMatchEvidence {
        match_confidence: u8::try_from(
            property("match_confidence").and_then(value_non_negative_i64)?,
        )
        .ok()?,
        simhash_distance: property("simhash_distance")
            .and_then(value_non_negative_i64)
            .and_then(|value| u32::try_from(value).ok()),
        matched_at: property("matched_at").and_then(value_non_empty)?,
        precaution: property("precaution").and_then(value_non_empty),
    })
}

pub(super) fn selective_adjacency(
    database: &GrafeoDB,
    element_nodes: &[NodeId],
    project_root: &str,
) -> (Vec<SemanticRelationship>, Vec<NodeId>) {
    let adjacency = element_nodes
        .iter()
        .flat_map(|source| {
            database
                .graph_store()
                .edges_from(*source, Direction::Outgoing)
                .into_iter()
                .map(|(target, edge)| (*source, target, edge))
        })
        .collect::<Vec<_>>();
    let edge_ids = adjacency
        .iter()
        .map(|(_, _, edge)| *edge)
        .collect::<Vec<_>>();
    let edge_keys = COMPLETE_EDGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_edge_batch_call();
    let properties = database
        .graph_store()
        .get_edges_properties_selective_batch(&edge_ids, &edge_keys);
    let mut relationships = Vec::new();
    let mut artifact_nodes = Vec::new();
    for ((_, target, _), properties) in adjacency.into_iter().zip(properties) {
        if properties
            .get("artifact_id")
            .and_then(value_string)
            .is_some()
        {
            artifact_nodes.push(target);
            continue;
        }
        if let Some(relationship) = relationship_from_properties(|key| properties.get(key))
            .filter(|relationship| relationship.project_root == project_root)
        {
            relationships.push(relationship);
        }
    }
    (relationships, artifact_nodes)
}

pub(super) fn selective_relationships(
    database: &GrafeoDB,
    edge_ids: &[EdgeId],
) -> Vec<SemanticRelationship> {
    let keys = COMPLETE_EDGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_edge_batch_call();
    database
        .graph_store()
        .get_edges_properties_selective_batch(edge_ids, &keys)
        .iter()
        .filter_map(|row| relationship_from_properties(|key| row.get(key)))
        .collect()
}

fn relationship_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticRelationship> {
    Some(SemanticRelationship {
        project_root: property("project_root").and_then(value_string)?,
        source_element_id: property("source_element_id").and_then(value_string)?,
        target_element_id: property("target_element_id").and_then(value_string)?,
        relationship_kind: property("relationship_kind").and_then(value_string)?,
        label: property("label").and_then(value_string)?,
        metadata: property("metadata_json")
            .map(value_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(super) fn semantic_owner_ids(database: &GrafeoDB, node_ids: &[NodeId]) -> HashSet<String> {
    let store = database.graph_store();
    selective_node_property_batch(&*store, node_ids, SEMANTIC_ELEMENT_ID_PROPERTY)
        .into_iter()
        .filter_map(|value| value.and_then(|value| value_string(&value)))
        .collect()
}

pub(super) fn selective_artifacts_for_owners(
    database: &GrafeoDB,
    owner_ids: HashSet<String>,
    artifact_namespace: Option<&str>,
) -> Vec<SemanticArtifact> {
    let store = database.graph_store();
    let artifact_nodes = store.nodes_by_label("SemanticArtifact");
    let owners =
        store.get_node_property_batch(&artifact_nodes, &SEMANTIC_ELEMENT_ID_PROPERTY.into());
    // Select owners before hydrating content: element nodes may carry large extractor metadata.
    let node_ids = artifact_nodes
        .into_iter()
        .zip(owners)
        .filter_map(|(id, owner)| {
            owner
                .and_then(|value| value_string(&value))
                .filter(|owner| owner_ids.contains(owner))
                .map(|_| id)
        })
        .collect::<Vec<_>>();
    let keys = COMPLETE_ARTIFACT_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_node_batch_call();
    let rows = store.get_nodes_properties_selective_batch(&node_ids, &keys);
    let mut seen = HashSet::new();
    node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, _)| {
            let artifact = artifact_from_properties(|key| {
                rows.get(row).and_then(|properties| properties.get(key))
            })?;
            (owner_ids.contains(&artifact.semantic_element_id)
                && artifact.metadata["association_kind"] != "inherited"
                && artifact_namespace
                    .is_none_or(|namespace| artifact.metadata[namespace].is_object())
                && seen.insert(artifact.artifact_id.clone()))
            .then_some(artifact)
        })
        .collect()
}

pub(super) fn selective_artifacts(
    database: &GrafeoDB,
    elements: &[SemanticElement],
    edge_artifact_nodes: &[NodeId],
    artifact_namespace: Option<&str>,
) -> Vec<SemanticArtifact> {
    let owner_ids = elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    let mut node_ids = edge_artifact_nodes.to_vec();
    for element_id in &owner_ids {
        node_ids.extend(database.find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(*element_id),
        ));
    }
    let mut seen_nodes = HashSet::new();
    node_ids.retain(|node_id| seen_nodes.insert(*node_id));
    let store = database.graph_store();
    let keys = COMPLETE_ARTIFACT_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_selective_node_batch_call();
    let rows = store.get_nodes_properties_selective_batch(&node_ids, &keys);
    let mut seen = HashSet::new();
    node_ids
        .iter()
        .enumerate()
        .filter_map(|(row, _)| {
            let artifact = artifact_from_properties(|key| {
                rows.get(row).and_then(|properties| properties.get(key))
            })?;
            if !owner_ids.is_empty() && !owner_ids.contains(artifact.semantic_element_id.as_str()) {
                return None;
            }
            if artifact.metadata["association_kind"] == "inherited"
                || artifact_namespace
                    .is_some_and(|namespace| !artifact.metadata[namespace].is_object())
            {
                return None;
            }
            seen.insert(artifact.artifact_id.clone())
                .then_some(artifact)
        })
        .collect()
}

pub(super) fn artifact_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<SemanticArtifact> {
    Some(SemanticArtifact {
        artifact_id: property("artifact_id").and_then(value_string)?,
        semantic_element_id: property("semantic_element_id").and_then(value_string)?,
        artifact_kind: property("artifact_kind").and_then(value_string)?,
        title: property("title").and_then(value_string)?,
        content_ref: property("content_ref").and_then(value_non_empty),
        content: property("content").and_then(value_non_empty),
        searchable_text: property("searchable_text").and_then(value_non_empty),
        content_size_bytes: property("content_size_bytes")
            .and_then(value_non_negative_i64)
            .and_then(|value| usize::try_from(value).ok()),
        dependencies: property("dependencies_json")
            .map(value_json)
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        metadata: property("metadata_json")
            .map(value_json)
            .unwrap_or(serde_json::Value::Null),
    })
}

pub(crate) fn value_string(value: &GrafeoValue) -> Option<String> {
    match value {
        GrafeoValue::String(value) => Some(value.to_string()),
        GrafeoValue::Int64(value) => Some(value.to_string()),
        GrafeoValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn value_non_empty(value: &GrafeoValue) -> Option<String> {
    value_string(value).filter(|value| !value.is_empty())
}

pub(crate) fn value_non_negative_i64(value: &GrafeoValue) -> Option<i64> {
    match value {
        GrafeoValue::Int64(value) if *value >= 0 => Some(*value),
        GrafeoValue::String(value) => value.parse().ok().filter(|value| *value >= 0),
        _ => None,
    }
}

pub(super) fn value_json(value: &GrafeoValue) -> serde_json::Value {
    value_non_empty(value)
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(serde_json::Value::Null)
}
#[cfg(test)]
pub(crate) fn semantic_elements_for_project(
    database: &GrafeoDB,
    project_root: &str,
) -> Vec<SemanticElement> {
    semantic_elements_for_project_including_inactive(database, project_root)
        .into_iter()
        .filter(|element| element.lifecycle != "inactive")
        .collect()
}

pub(crate) fn semantic_elements_for_project_including_inactive(
    database: &GrafeoDB,
    project_root: &str,
) -> Vec<SemanticElement> {
    let semantic_node_ids = database
        .graph_store()
        .nodes_by_label("SemanticElement")
        .into_iter()
        .collect::<HashSet<_>>();
    let node_ids = database
        .find_nodes_by_property(PROJECT_ROOT_PROPERTY, &GrafeoValue::from(project_root))
        .into_iter()
        .filter(|node_id| semantic_node_ids.contains(node_id))
        .collect::<Vec<_>>();
    selective_elements(database, &node_ids, Some(project_root), true)
}

pub(crate) fn semantic_element_by_id(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Option<SemanticElement> {
    database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .find(|node| node.has_label("SemanticElement") && element_node_is_active(node))
        .and_then(|node| semantic_element_from_node(&node))
}

pub(crate) fn semantic_artifacts_for_elements(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Vec<SemanticArtifact> {
    let mut artifacts = BTreeMap::new();
    for artifact in semantic_artifacts_for_elements_from_edges(database, semantic_element_ids) {
        artifacts.insert(artifact.artifact_id.clone(), artifact);
    }
    for artifact in semantic_artifacts_for_elements_by_property(database, semantic_element_ids) {
        artifacts
            .entry(artifact.artifact_id.clone())
            .or_insert(artifact);
    }
    artifacts.into_values().collect()
}

pub(super) fn semantic_artifacts_for_elements_by_property(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Vec<SemanticArtifact> {
    selective_artifacts_for_owners(database, semantic_element_ids.clone(), None)
}

pub(crate) fn semantic_artifacts_for_element(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Vec<SemanticArtifact> {
    let mut element_ids = HashSet::with_capacity(1);
    element_ids.insert(semantic_element_id.to_string());
    semantic_artifacts_for_elements(database, &element_ids)
}

pub(crate) fn semantic_artifact_by_id(
    database: &GrafeoDB,
    artifact_id: &str,
) -> Option<SemanticArtifact> {
    database
        .find_nodes_by_property(ARTIFACT_ID_PROPERTY, &GrafeoValue::from(artifact_id))
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .find(|node| node.has_label("SemanticArtifact"))
        .and_then(|node| semantic_artifact_from_node(&node))
}

/// Looks up nodes carrying `label` via the `property = value` index instead of a full scan.
pub(crate) fn node_ids_by_label_and_property(
    database: &GrafeoDB,
    label: &str,
    property: &str,
    value: &str,
) -> Vec<NodeId> {
    // Bulk project operations need identities, not cloned metadata/vector properties.
    if property == PROJECT_ROOT_PROPERTY {
        let labeled: HashSet<_> = database
            .graph_store()
            .nodes_by_label(label)
            .into_iter()
            .collect();
        return database
            .find_nodes_by_property(property, &GrafeoValue::from(value))
            .into_iter()
            .filter(|id| labeled.contains(id))
            .collect();
    }
    database
        .find_nodes_by_property(property, &GrafeoValue::from(value))
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .filter(|node| node.has_label(label))
        .filter(|node| string_property(node, property).as_deref() == Some(value))
        .map(|node| node.id)
        .collect()
}

pub(crate) fn nodes_by_label_and_property(
    database: &GrafeoDB,
    label: &str,
    property: &str,
    value: &str,
) -> Vec<grafeo_core::graph::lpg::Node> {
    database
        .find_nodes_by_property(property, &GrafeoValue::from(value))
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .filter(|node| node.has_label(label))
        .collect()
}

#[cfg(test)]
pub(crate) fn delete_nodes_by_property(
    database: &GraphTransaction<'_>,
    label: &str,
    property: &str,
    value: &str,
) -> usize {
    let node_ids = database
        .find_nodes_by_property(property, &GrafeoValue::from(value))
        .into_iter()
        .filter_map(|node_id| database.get_node(node_id))
        .filter(|node| node.has_label(label))
        .filter(|node| string_property(node, property).as_deref() == Some(value))
        .map(|node| node.id)
        .collect::<Vec<_>>();
    let count = node_ids.len();
    for node_id in node_ids {
        database.delete_node(node_id);
    }
    count
}

pub(super) fn relationship_edge_touches_element(
    edge: &grafeo_core::graph::lpg::Edge,
    semantic_element_id: &str,
) -> bool {
    edge_string_property(edge, "source_element_id").as_deref() == Some(semantic_element_id)
        || edge_string_property(edge, "target_element_id").as_deref() == Some(semantic_element_id)
}
pub(super) fn semantic_artifacts_for_elements_from_edges(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Vec<SemanticArtifact> {
    active_element_nodes(database, semantic_element_ids)
        .into_iter()
        .flat_map(|node| database.graph_store().edges_from(node, Direction::Outgoing))
        .filter_map(|(_, edge_id)| database.get_edge(edge_id))
        .filter(|edge| edge.edge_type == SEMANTIC_ARTIFACT_EDGE_TYPE)
        .filter_map(|edge| database.get_node(edge.dst))
        .filter(|node| node.has_label("SemanticArtifact"))
        .filter_map(|node| semantic_artifact_from_node(&node))
        .filter(|artifact| artifact.metadata["association_kind"] != "inherited")
        .collect()
}

pub(super) fn active_element_nodes(
    database: &GrafeoDB,
    semantic_element_ids: &HashSet<String>,
) -> Vec<NodeId> {
    let labels = database
        .graph_store()
        .nodes_by_label("SemanticElement")
        .into_iter()
        .collect::<HashSet<_>>();
    let nodes = semantic_element_ids
        .iter()
        .flat_map(|id| {
            database.find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from(id.as_str()),
            )
        })
        .filter(|id| labels.contains(id))
        .collect::<Vec<_>>();
    let rows = database
        .graph_store()
        .get_nodes_properties_selective_batch(&nodes, &["active".into(), "lifecycle".into()]);
    nodes
        .into_iter()
        .zip(rows)
        .filter(|(_, row)| active_properties(row.get("active"), row.get("lifecycle")))
        .map(|(node, _)| node)
        .collect()
}

pub(super) fn semantic_element_node_id(
    database: &GrafeoDB,
    semantic_element_id: &str,
) -> Option<NodeId> {
    let direct_id = semantic_element_node_id_for(semantic_element_id);
    if semantic_element_id_matches(database, direct_id, semantic_element_id) {
        return Some(direct_id);
    }
    database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .into_iter()
        .find(|node_id| {
            database.get_node(*node_id).is_some_and(|node| {
                node.has_label("SemanticElement") && element_node_is_active(&node)
            })
        })
}

#[cfg(test)]
pub(super) fn transaction_semantic_element_node_id(
    database: &GraphTransaction<'_>,
    semantic_element_id: &str,
) -> Option<NodeId> {
    if let Some(node_id) = database.semantic_node_id(semantic_element_id) {
        return Some(node_id);
    }
    database
        .find_nodes_by_property(
            SEMANTIC_ELEMENT_ID_PROPERTY,
            &GrafeoValue::from(semantic_element_id),
        )
        .into_iter()
        .find(|node_id| {
            database
                .get_node(*node_id)
                .is_some_and(|node| node.has_label("SemanticElement"))
        })
}

pub(super) fn element_node_is_active(node: &grafeo_core::graph::lpg::Node) -> bool {
    active_properties(
        node.get_property(ACTIVE_PROPERTY),
        node.get_property("lifecycle"),
    )
}

pub(super) fn active_properties(
    active: Option<&GrafeoValue>,
    lifecycle: Option<&GrafeoValue>,
) -> bool {
    match active {
        Some(GrafeoValue::Bool(active)) => *active,
        Some(GrafeoValue::String(active)) => active == "true",
        Some(GrafeoValue::Int64(active)) => *active != 0,
        Some(_) => false,
        None => lifecycle.and_then(value_string).as_deref() != Some("inactive"),
    }
}

pub(super) fn semantic_element_id_matches(
    database: &GrafeoDB,
    node_id: NodeId,
    semantic_element_id: &str,
) -> bool {
    database.get_node(node_id).is_some_and(|node| {
        node.has_label("SemanticElement")
            && element_node_is_active(&node)
            && string_property(&node, SEMANTIC_ELEMENT_ID_PROPERTY).as_deref()
                == Some(semantic_element_id)
    })
}
