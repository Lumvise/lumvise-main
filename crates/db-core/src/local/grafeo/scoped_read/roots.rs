//! Project-root selection reads adjacency columns, never relationship metadata.
use crate::Result;
use crate::local::grafeo::committed_columns::CommittedPropertyRows;
use crate::local::grafeo::graph_reads::{CommittedGraph, GrafeoResult};
use std::collections::HashSet;

use grafeo::{EdgeId, GrafeoDB, Value};
use grafeo_core::graph::Direction;

use super::super::graph_rows::{node_ids_by_label_and_property, project_member_ids, value_string};

pub(super) fn project_roots(
    graph: &GrafeoDB,
    project_root: &str,
    include_inactive: bool,
) -> Result<Vec<String>> {
    let members = project_member_ids(graph, project_root, include_inactive)?;
    let parented = parented_members(graph, project_root, &members)?;
    Ok(members.difference(&parented).cloned().collect())
}

fn parented_members(
    graph: &GrafeoDB,
    project_root: &str,
    members: &HashSet<String>,
) -> Result<HashSet<String>> {
    let incoming = incoming_member_edges(graph, project_root, members)?;
    let edges = incoming.iter().map(|(_, edge)| *edge).collect::<Vec<_>>();
    let keys = [
        "project_root",
        "source_element_id",
        "target_element_id",
        "relationship_kind",
        "label",
    ]
    .map(Into::into);
    let snapshot = graph.committed_snapshot()?;
    let properties = snapshot
        .edge_properties_selective_batch(&edges, &keys, snapshot.view())
        .db()?
        .committed_edges(&snapshot, &edges)?;
    Ok(incoming
        .into_iter()
        .zip(properties)
        .filter_map(|((target, _), row)| {
            has_member_parent(|key| row.get(key), project_root, members).then_some(target)
        })
        .collect())
}

fn incoming_member_edges(
    graph: &GrafeoDB,
    project_root: &str,
    members: &HashSet<String>,
) -> Result<Vec<(String, EdgeId)>> {
    let snapshot = graph.committed_snapshot()?;
    let nodes =
        node_ids_by_label_and_property(graph, "SemanticElement", "project_root", project_root)?;
    let identities = snapshot
        .node_property_batch(&nodes, &"semantic_element_id".into(), snapshot.view())
        .db()?
        .committed_nodes(&snapshot, &nodes)?;
    let mut incoming = Vec::new();
    for (node, identity) in nodes.into_iter().zip(identities) {
        let Some(id) = identity
            .as_ref()
            .and_then(value_string)
            .filter(|id| members.contains(id))
        else {
            continue;
        };
        // GRAFEO06-REVIEW(23): committed incoming adjacency; 0.5 edges_from read the live store.
        incoming.extend(
            snapshot
                .edges(node, Direction::Incoming, &[], snapshot.view())
                .db()?
                .into_iter()
                .map(|(_, edge)| (id.clone(), edge)),
        );
    }
    Ok(incoming)
}

fn has_member_parent<'a>(
    property: impl Fn(&str) -> Option<&'a Value>,
    project_root: &str,
    members: &HashSet<String>,
) -> bool {
    let string = |key| property(key).and_then(value_string);
    let (Some(root), Some(source), Some(_target), Some(kind), Some(label)) = (
        string("project_root"),
        string("source_element_id"),
        string("target_element_id"),
        string("relationship_kind"),
        string("label"),
    ) else {
        return false;
    };
    root == project_root && members.contains(&source) && (kind == "contains" || label == "contains")
}
