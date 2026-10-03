use super::*;
struct NodePropertyColumns {
    rows: Vec<HashMap<String, GrafeoValue>>,
}

impl NodePropertyColumns {
    fn for_elements(store: &dyn GraphStore, ids: &[NodeId]) -> Self {
        Self::selected(
            store,
            ids,
            &[
                "semantic_element_id",
                "path",
                "element_kind",
                "name",
                "start_line",
                "end_line",
                "lifecycle",
                "metadata_json",
            ],
        )
    }

    fn for_artifacts(store: &dyn GraphStore, ids: &[NodeId]) -> Self {
        Self::selected(
            store,
            ids,
            &[
                "artifact_id",
                "semantic_element_id",
                "artifact_kind",
                "title",
                "content_ref",
                "content",
                "searchable_text",
                "content_size_bytes",
                "metadata_json",
            ],
        )
    }

    fn selected(store: &dyn GraphStore, ids: &[NodeId], keys: &[&str]) -> Self {
        let keys = keys.iter().map(|key| (*key).into()).collect::<Vec<_>>();
        let rows = store
            .get_nodes_properties_selective_batch(ids, &keys)
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect()
            })
            .collect();
        Self { rows }
    }

    fn element_property(&self, row: usize, key: &str) -> Option<&GrafeoValue> {
        self.rows.get(row)?.get(key)
    }

    fn artifact_property(&self, row: usize, key: &str) -> Option<&GrafeoValue> {
        self.rows.get(row)?.get(key)
    }
}
pub(super) fn read_batched_rows(graph: &GrafeoDB, project_root: &str) -> BatchedGraphRows {
    let store = graph.graph_store();
    let semantic_label_ids = store.nodes_by_label("SemanticElement");
    let semantic_node_ids = semantic_label_ids.iter().copied().collect::<HashSet<_>>();
    let semantic_id_key = "semantic_element_id".into();
    let semantic_ids_by_node_all = semantic_label_ids
        .iter()
        .copied()
        .zip(store.get_node_property_batch(&semantic_label_ids, &semantic_id_key))
        .filter_map(|(node_id, value)| {
            value
                .as_ref()
                .and_then(|v| value_string(Some(v)))
                .map(|id| (node_id, id))
        })
        .collect::<HashMap<_, _>>();
    let candidate_ids = store
        .find_nodes_by_properties(&[
            ("project_root", GrafeoValue::from(project_root)),
            ("active", GrafeoValue::from(true)),
        ])
        .into_iter()
        .filter(|node_id| semantic_node_ids.contains(node_id))
        .collect::<Vec<_>>();
    let element_columns = NodePropertyColumns::for_elements(&*store, &candidate_ids);
    let mut all = Vec::with_capacity(candidate_ids.len());
    let mut node_ids = HashMap::with_capacity(candidate_ids.len());
    let mut semantic_ids_by_node = HashMap::with_capacity(candidate_ids.len());
    for (row, node_id) in candidate_ids.iter().copied().enumerate() {
        let Some(element) =
            projection_element_from_properties(|key| element_columns.element_property(row, key))
        else {
            continue;
        };
        semantic_ids_by_node.insert(node_id, element.semantic_element_id.clone());
        node_ids.insert(element.semantic_element_id.clone(), node_id);
        all.push(element);
    }
    let all_by_id = all
        .iter()
        .cloned()
        .map(|element| (element.semantic_element_id.clone(), element))
        .collect::<HashMap<_, _>>();

    let mut adjacency = Vec::<(NodeId, NodeId, EdgeId)>::new();
    let mut seen_edges = HashSet::new();
    for source_id in node_ids.values().copied() {
        for (target_id, edge_id) in store.edges_from(source_id, Direction::Outgoing) {
            if seen_edges.insert(edge_id) {
                adjacency.push((source_id, target_id, edge_id));
            }
        }
    }
    let edge_ids = adjacency
        .iter()
        .map(|(_, _, edge_id)| *edge_id)
        .collect::<Vec<_>>();
    let edge_keys = EDGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    let edge_properties = store.get_edges_properties_selective_batch(&edge_ids, &edge_keys);
    let mut relationships = Vec::new();
    let mut artifact_nodes = HashMap::<NodeId, String>::new();
    for ((source_node, target_node, _), properties) in adjacency.into_iter().zip(edge_properties) {
        let Some(owner) = semantic_ids_by_node_all.get(&source_node).cloned() else {
            continue;
        };
        if let Some(target) = semantic_ids_by_node_all.get(&target_node).cloned() {
            let Some(relationship_kind) = value_string(properties.get("relationship_kind")) else {
                continue;
            };
            let Some(label) = value_string(properties.get("label")) else {
                continue;
            };
            relationships.push(SemanticRelationship {
                project_root: project_root.to_owned(),
                source_element_id: owner,
                target_element_id: target,
                relationship_kind,
                label,
                metadata: serde_json::Value::Null,
            });
        } else {
            artifact_nodes.insert(target_node, owner);
        }
    }
    relationships.sort_by(|left, right| {
        left.source_element_id
            .cmp(&right.source_element_id)
            .then(left.target_element_id.cmp(&right.target_element_id))
            .then(left.relationship_kind.cmp(&right.relationship_kind))
            .then(left.label.cmp(&right.label))
    });

    let mut artifacts_by_element = HashMap::new();
    let artifact_node_ids = artifact_nodes.keys().copied().collect::<Vec<_>>();
    if !artifact_node_ids.is_empty() {
        let mut seen_artifacts = HashSet::<(String, String)>::new();
        let artifact_columns = NodePropertyColumns::for_artifacts(&*store, &artifact_node_ids);
        for (row, node_id) in artifact_node_ids.iter().copied().enumerate() {
            let artifact = projection_artifact_from_properties(|key| {
                artifact_columns.artifact_property(row, key)
            });
            let Some(artifact) = artifact else {
                continue;
            };
            if artifact.metadata["association_kind"] == "inherited" {
                continue;
            }
            let Some(owner) = artifact_nodes.get(&node_id) else {
                continue;
            };
            if artifact.semantic_element_id != *owner
                || !seen_artifacts.insert((owner.clone(), artifact.artifact_id.clone()))
            {
                continue;
            }
            artifacts_by_element
                .entry(owner.clone())
                .or_insert_with(Vec::new)
                .push(artifact);
        }
    }
    BatchedGraphRows {
        all,
        all_by_id,
        relationships,
        artifacts_by_element,
    }
}

pub(super) fn projection_element_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<ProjectionElement> {
    Some(ProjectionElement {
        semantic_element_id: value_string(property("semantic_element_id"))?,
        path: value_string(property("path"))?,
        element_kind: value_string(property("element_kind"))?,
        name: value_string(property("name"))?,
        start_line: non_negative_i64(property("start_line")),
        end_line: non_negative_i64(property("end_line")),
        lifecycle: value_string(property("lifecycle")).unwrap_or_else(|| "active".to_string()),
        metadata: json_value(property("metadata_json")),
    })
}

pub(super) fn projection_artifact_from_properties<'a>(
    property: impl Fn(&str) -> Option<&'a GrafeoValue>,
) -> Option<ProjectionArtifact> {
    Some(ProjectionArtifact {
        artifact_id: value_string(property("artifact_id"))?,
        semantic_element_id: value_string(property("semantic_element_id"))?,
        artifact_kind: value_string(property("artifact_kind"))?,
        title: value_string(property("title"))?,
        content_ref: non_empty_string(property("content_ref")),
        content: non_empty_string(property("content")),
        searchable_text: non_empty_string(property("searchable_text")),
        content_size_bytes: non_negative_i64(property("content_size_bytes"))
            .and_then(|value| usize::try_from(value).ok()),
        metadata: json_value(property("metadata_json")),
    })
}

pub(super) fn value_string(value: Option<&GrafeoValue>) -> Option<String> {
    match value {
        Some(GrafeoValue::String(value)) => Some(value.to_string()),
        Some(GrafeoValue::Int64(value)) => Some(value.to_string()),
        Some(GrafeoValue::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn non_empty_string(value: Option<&GrafeoValue>) -> Option<String> {
    value_string(value).filter(|value| !value.is_empty())
}

pub(super) fn non_negative_i64(value: Option<&GrafeoValue>) -> Option<i64> {
    value_string(value)?
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
}

pub(super) fn json_value(value: Option<&GrafeoValue>) -> serde_json::Value {
    non_empty_string(value)
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or(serde_json::Value::Null)
}
