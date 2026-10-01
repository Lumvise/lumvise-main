//! Request-scoped descendant selection; only selected records are hydrated, once.
use super::records::{active_properties, semantic_element_node_id, value_string};
use super::{PARENT_ELEMENT_ID_PROPERTY, SEMANTIC_ELEMENT_ID_PROPERTY, selective_elements};
use crate::SemanticElement;
use grafeo::{GrafeoDB, NodeId, Value};
use grafeo_core::graph::Direction;
use std::collections::{BTreeMap, HashSet};

pub(crate) fn subtree_elements(
    graph: &GrafeoDB,
    root: SemanticElement,
    maximum_depth: usize,
) -> Vec<SemanticElement> {
    if maximum_depth == 0 {
        return vec![root];
    }
    let Some(root_node) = semantic_element_node_id(graph, &root.semantic_element_id) else {
        return vec![root];
    };
    let mut selection = SubtreeSelection {
        graph,
        element_nodes: graph
            .graph_store()
            .nodes_by_label("SemanticElement")
            .into_iter()
            .collect(),
        visited: HashSet::from([root.semantic_element_id.clone()]),
        descendants: Vec::new(),
    };
    selection.walk(
        vec![(root_node, root.semantic_element_id.clone())],
        maximum_depth,
    );
    let mut elements = vec![root];
    elements.extend(selective_elements(
        graph,
        &selection.descendants,
        None,
        true,
    ));
    elements
}

struct SubtreeSelection<'graph> {
    graph: &'graph GrafeoDB,
    element_nodes: HashSet<NodeId>,
    visited: HashSet<String>,
    descendants: Vec<NodeId>,
}

impl SubtreeSelection<'_> {
    fn walk(&mut self, mut frontier: Vec<(NodeId, String)>, maximum_depth: usize) {
        for _ in 0..maximum_depth {
            let candidates = self.child_nodes(&frontier);
            frontier = self.active_children(candidates);
            if frontier.is_empty() {
                break;
            }
            self.descendants
                .extend(frontier.iter().map(|(node, _)| *node));
        }
    }

    fn child_nodes(&self, parents: &[(NodeId, String)]) -> Vec<NodeId> {
        let mut children = HashSet::new();
        for (node, id) in parents {
            children.extend(
                self.graph
                    .find_nodes_by_property(PARENT_ELEMENT_ID_PROPERTY, &Value::from(id.as_str())),
            );
            children.extend(self.edge_children(*node));
        }
        children
            .into_iter()
            .filter(|node| self.element_nodes.contains(node))
            .collect()
    }

    fn edge_children(&self, parent: NodeId) -> Vec<NodeId> {
        let store = self.graph.graph_store();
        let adjacency = store.edges_from(parent, Direction::Outgoing);
        let edges = adjacency.iter().map(|(_, edge)| *edge).collect::<Vec<_>>();
        let properties = store.get_edges_properties_selective_batch(
            &edges,
            &["relationship_kind".into(), "label".into()],
        );
        adjacency
            .into_iter()
            .zip(properties)
            .filter_map(|((target, _), row)| {
                ["relationship_kind", "label"]
                    .iter()
                    .any(|key| row.get(*key).and_then(value_string).as_deref() == Some("contains"))
                    .then_some(target)
            })
            .collect()
    }

    fn active_children(&mut self, nodes: Vec<NodeId>) -> Vec<(NodeId, String)> {
        let keys = [
            SEMANTIC_ELEMENT_ID_PROPERTY.into(),
            "active".into(),
            "lifecycle".into(),
        ];
        let rows = self
            .graph
            .graph_store()
            .get_nodes_properties_selective_batch(&nodes, &keys);
        let mut identities = BTreeMap::new();
        for (node, row) in nodes.into_iter().zip(rows) {
            if let Some(id) = active_identity(|key| row.get(key)) {
                identities.insert(id, node);
            }
        }
        identities
            .into_iter()
            .filter(|(id, _)| self.visited.insert(id.clone()))
            .map(|(id, node)| (node, id))
            .collect()
    }
}

fn active_identity<'a>(property: impl Fn(&str) -> Option<&'a Value>) -> Option<String> {
    let active = active_properties(property("active"), property("lifecycle"));
    active
        .then(|| property(SEMANTIC_ELEMENT_ID_PROPERTY).and_then(value_string))
        .flatten()
}
