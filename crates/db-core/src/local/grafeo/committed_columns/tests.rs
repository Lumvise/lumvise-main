//! Committed column reads keep positional alignment while hiding rolled-back properties.
use super::*;
use crate::local::grafeo::graph_store::{GraphStore as LocalGraphStore, GraphTransaction};
use crate::{DbError, Result};
use grafeo::{GrafeoDB, Value};
use std::cell::Cell;

fn pending_columns(tx: &GraphTransaction<'_>, visible: NodeId) -> Result<(NodeId, EdgeId)> {
    let property = [("name", Value::from("hidden"))];
    let hidden = tx.create_node_with_props(&["ColumnTest"], property)?;
    let relationship = [("name", Value::from("hidden edge"))];
    let edge = tx.create_edge_with_props(visible, hidden, "TEST", relationship)?;
    Ok((hidden, edge))
}

fn rollback_columns() -> (LocalGraphStore, [NodeId; 2], EdgeId) {
    let graph = LocalGraphStore::new(GrafeoDB::new_in_memory());
    let visible = graph
        .write(|tx| tx.create_node_with_props(&["ColumnTest"], [("name", Value::from("visible"))]))
        .unwrap();
    let pending = Cell::new((NodeId(0), EdgeId(0)));
    let failed: Result<()> = graph.write(|tx| {
        pending.set(pending_columns(tx, visible)?);
        Err(DbError::invalid_value(
            "rolled back columns",
            "committed columns",
        ))
    });
    assert!(failed.is_err());
    let (hidden, edge) = pending.get();
    (graph, [visible, hidden], edge)
}

#[test]
fn node_columns_keep_positions_and_hide_rolled_back_properties() {
    let (graph, nodes, _) = rollback_columns();
    graph.read(|database| {
        let store = database.graph_store();
        let rows = store
            .get_nodes_properties_selective_batch(&nodes, &["name".into()])
            .committed_nodes(&*store, &nodes);
        assert_eq!(rows[0].get("name"), Some(&Value::from("visible")));
        assert!(rows[1].is_empty());
    });
}

#[test]
fn node_values_keep_positions_and_hide_rolled_back_properties() {
    let (graph, nodes, _) = rollback_columns();
    graph.read(|database| {
        let store = database.graph_store();
        let values = store
            .get_node_property_batch(&nodes, &"name".into())
            .committed_nodes(&*store, &nodes);
        assert_eq!(values, vec![Some(Value::from("visible")), None]);
    });
}

#[test]
fn edge_columns_hide_rolled_back_relationship_properties() {
    let (graph, _, edge) = rollback_columns();
    graph.read(|database| {
        let store = database.graph_store();
        let rows = store
            .get_edges_properties_selective_batch(&[edge], &["name".into()])
            .committed_edges(&*store, &[edge]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_empty());
    });
}
