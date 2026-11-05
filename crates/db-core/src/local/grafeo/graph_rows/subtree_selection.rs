//! Request-scoped descendant selection; only selected records are hydrated, once.
use super::records::{active_properties, semantic_element_node_id, value_string};
use super::{PARENT_ELEMENT_ID_PROPERTY, SEMANTIC_ELEMENT_ID_PROPERTY, selective_current_elements};
use crate::local::grafeo::committed_columns::CommittedPropertyRows;
use crate::local::grafeo::graph_reads::{CommittedGraph, GrafeoResult};
use crate::{Result, SemanticElement};
use grafeo::{GrafeoDB, NodeId, Snapshot, Value};
use grafeo_core::graph::Direction;
use std::collections::{BTreeMap, HashSet};

pub(crate) fn subtree_elements(
    graph: &GrafeoDB,
    root: SemanticElement,
    maximum_depth: usize,
) -> Result<Vec<SemanticElement>> {
    if maximum_depth == 0 {
        return Ok(vec![root]);
    }
    let Some(root_node) = semantic_element_node_id(graph, &root.semantic_element_id)? else {
        return Ok(vec![root]);
    };
    let snapshot = graph.committed_snapshot()?;
    let element_nodes = snapshot
        .nodes_by_label("SemanticElement", snapshot.view())
        .db()?
        .into_iter()
        .collect();
    let mut selection = SubtreeSelection {
        graph,
        snapshot,
        element_nodes,
        visited: HashSet::from([root.semantic_element_id.clone()]),
        descendants: Vec::new(),
    };
    selection.walk(
        vec![(root_node, root.semantic_element_id.clone())],
        maximum_depth,
    )?;
    let mut elements = vec![root];
    elements.extend(selective_current_elements(
        graph,
        &selection.descendants,
        None,
        true,
    )?);
    Ok(elements)
}

struct SubtreeSelection<'graph> {
    graph: &'graph GrafeoDB,
    snapshot: Snapshot,
    element_nodes: HashSet<NodeId>,
    visited: HashSet<String>,
    descendants: Vec<NodeId>,
}

impl SubtreeSelection<'_> {
    fn walk(&mut self, mut frontier: Vec<(NodeId, String)>, maximum_depth: usize) -> Result<()> {
        for _ in 0..maximum_depth {
            let candidates = self.child_nodes(&frontier)?;
            frontier = self.active_children(candidates)?;
            if frontier.is_empty() {
                break;
            }
            self.descendants
                .extend(frontier.iter().map(|(node, _)| *node));
        }
        Ok(())
    }

    fn child_nodes(&self, parents: &[(NodeId, String)]) -> Result<Vec<NodeId>> {
        let mut children = HashSet::new();
        for (node, id) in parents {
            children.extend(
                self.graph
                    .find_nodes_by_property(PARENT_ELEMENT_ID_PROPERTY, &Value::from(id.as_str()))
                    .db()?,
            );
            children.extend(self.edge_children(*node)?);
        }
        Ok(children
            .into_iter()
            .filter(|node| self.element_nodes.contains(node))
            .collect())
    }

    fn edge_children(&self, parent: NodeId) -> Result<Vec<NodeId>> {
        let view = self.snapshot.view();
        // GRAFEO06-REVIEW(15): committed adjacency; 0.5 edges_from read the live store.
        let adjacency = self
            .snapshot
            .edges(parent, Direction::Outgoing, &[], view)
            .db()?;
        let edges = adjacency.iter().map(|(_, edge)| *edge).collect::<Vec<_>>();
        let properties = self
            .snapshot
            .edge_properties_selective_batch(
                &edges,
                &["relationship_kind".into(), "label".into()],
                view,
            )
            .db()?
            .committed_edges(&self.snapshot, &edges)?;
        Ok(adjacency
            .into_iter()
            .zip(properties)
            .filter_map(|((target, _), row)| {
                ["relationship_kind", "label"]
                    .iter()
                    .any(|key| row.get(*key).and_then(value_string).as_deref() == Some("contains"))
                    .then_some(target)
            })
            .collect())
    }

    fn active_children(&mut self, nodes: Vec<NodeId>) -> Result<Vec<(NodeId, String)>> {
        let keys = [
            SEMANTIC_ELEMENT_ID_PROPERTY.into(),
            "active".into(),
            "lifecycle".into(),
        ];
        let rows = self
            .snapshot
            .node_properties_selective_batch(&nodes, &keys, self.snapshot.view())
            .db()?
            .committed_nodes(&self.snapshot, &nodes)?;
        let mut identities = BTreeMap::new();
        for (node, row) in nodes.into_iter().zip(rows) {
            if let Some(id) = active_identity(|key| row.get(key)) {
                identities.insert(id, node);
            }
        }
        Ok(identities
            .into_iter()
            .filter(|(id, _)| self.visited.insert(id.clone()))
            .map(|(id, node)| (node, id))
            .collect())
    }
}

fn active_identity<'a>(property: impl Fn(&str) -> Option<&'a Value>) -> Option<String> {
    let active = active_properties(property("active"), property("lifecycle"));
    active
        .then(|| property(SEMANTIC_ELEMENT_ID_PROPERTY).and_then(value_string))
        .flatten()
}
