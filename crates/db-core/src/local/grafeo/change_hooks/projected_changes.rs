//! Change delivery needs identity and revision fields, never metadata or vectors.

use crate::Result;
use crate::local::grafeo::committed_columns::CommittedPropertyRows;
use crate::local::grafeo::graph_reads::{CommittedGraph, GrafeoResult};
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

pub(super) fn project_change_nodes(graph: &GrafeoDB, project_root: &str) -> Result<Vec<Node>> {
    let ids = project_element_node_ids(graph, project_root)?;
    let snapshot = graph.committed_snapshot()?;
    let keys = CHANGE_PROPERTIES
        .iter()
        .map(|key| (*key).into())
        .collect::<Vec<_>>();
    let rows = snapshot
        .node_properties_selective_batch(&ids, &keys, snapshot.view())
        .db()?
        .committed_nodes(&snapshot, &ids)?;
    Ok(ids
        .into_iter()
        .zip(rows)
        .filter(|(_, properties)| !properties.is_empty())
        .map(|(id, properties)| {
            let mut node = Node::with_labels(id, ["SemanticElement"]);
            node.properties = properties.into_iter().collect();
            node
        })
        .collect())
}

fn project_element_node_ids(graph: &GrafeoDB, project_root: &str) -> Result<Vec<grafeo::NodeId>> {
    let snapshot = graph.committed_snapshot()?;
    let semantic_ids = snapshot
        .nodes_by_label("SemanticElement", snapshot.view())
        .db()?
        .into_iter()
        .collect::<HashSet<_>>();
    Ok(graph
        .find_nodes_by_property("project_root", &Value::from(project_root))
        .db()?
        .into_iter()
        .filter(|id| semantic_ids.contains(id))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_projection_excludes_other_projects_vectors_and_bulk_properties() {
        let graph = GrafeoDB::new_in_memory();
        let element = graph.create_node(&["SemanticElement"]).unwrap();
        graph
            .set_node_property(element, "project_root", "/one".into())
            .unwrap();
        graph
            .set_node_property(element, "semantic_element_id", "owner".into())
            .unwrap();
        graph
            .set_node_property(element, "last_changed_revision", 7_i64.into())
            .unwrap();
        graph
            .set_node_property(
                element,
                "metadata_json",
                "bulk metadata".repeat(10_000).into(),
            )
            .unwrap();
        let vector = graph.create_node(&["SemanticElementNameVector"]).unwrap();
        graph
            .set_node_property(vector, "project_root", "/one".into())
            .unwrap();
        let other = graph.create_node(&["SemanticElement"]).unwrap();
        graph
            .set_node_property(other, "project_root", "/two".into())
            .unwrap();
        let nodes = project_change_nodes(&graph, "/one").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, element);
        assert_eq!(
            nodes[0].get_property("last_changed_revision"),
            Some(&Value::Int64(7))
        );
        assert!(nodes[0].get_property("metadata_json").is_none());
    }
}
