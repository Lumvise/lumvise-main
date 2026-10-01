//! Change delivery needs identity and revision fields, never metadata or vectors.

use grafeo::{GrafeoDB, Value};
use grafeo_core::graph::lpg::Node;
use std::collections::HashSet;

const CHANGE_PROPERTIES: &[&str] = &[
    "project_root",
    "semantic_element_id",
    "semantic_source_id",
    "path",
    "element_kind",
    "name",
    "last_changed_revision",
    "active",
];

pub(super) fn project_change_nodes(graph: &GrafeoDB, project_root: &str) -> Vec<Node> {
    let ids = project_element_node_ids(graph, project_root);
    let store = graph.graph_store();
    let keys = CHANGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    let rows = store.get_nodes_properties_selective_batch(&ids, &keys);
    ids.into_iter()
        .zip(rows)
        .map(|(id, properties)| {
            let mut node = Node::with_labels(id, ["SemanticElement"]);
            node.properties = properties.into_iter().collect();
            node
        })
        .collect()
}

fn project_element_node_ids(graph: &GrafeoDB, project_root: &str) -> Vec<grafeo::NodeId> {
    let store = graph.graph_store();
    let semantic_ids = store
        .nodes_by_label("SemanticElement")
        .into_iter()
        .collect::<HashSet<_>>();
    graph
        .find_nodes_by_property("project_root", &Value::from(project_root))
        .into_iter()
        .filter(|id| semantic_ids.contains(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_projection_excludes_other_projects_vectors_and_bulk_properties() {
        let graph = GrafeoDB::new_in_memory();
        let element = graph.create_node(&["SemanticElement"]);
        graph.set_node_property(element, "project_root", "/one".into());
        graph.set_node_property(element, "semantic_element_id", "owner".into());
        graph.set_node_property(element, "last_changed_revision", 7_i64.into());
        graph.set_node_property(
            element,
            "metadata_json",
            "bulk metadata".repeat(10_000).into(),
        );
        let vector = graph.create_node(&["SemanticElementNameVector"]);
        graph.set_node_property(vector, "project_root", "/one".into());
        let other = graph.create_node(&["SemanticElement"]);
        graph.set_node_property(other, "project_root", "/two".into());
        let nodes = project_change_nodes(&graph, "/one");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, element);
        assert_eq!(
            nodes[0].get_property("last_changed_revision"),
            Some(&Value::Int64(7))
        );
        assert!(nodes[0].get_property("metadata_json").is_none());
    }
}
