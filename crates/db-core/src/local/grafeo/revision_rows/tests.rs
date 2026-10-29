use super::*;
use crate::local::grafeo::graph_store::GraphStore;
use std::cell::Cell;

fn revision_metadata_node(graph: &GraphTransaction<'_>) -> Result<NodeId> {
    graph.create_node_with_props(
        &[ROW_LABEL],
        [(
            ROW_PAYLOAD,
            Value::from(serde_json::to_string(&RevisionRow::Project(
                "lineage".into(),
            ))?),
        )],
    )
}

fn rolled_back_metadata_node(store: &GraphStore) -> NodeId {
    let created = Cell::new(None);
    let result: Result<()> = store.write(|graph| {
        created.set(Some(revision_metadata_node(graph)?));
        Err(DbError::invalid_value(
            "rolled back metadata",
            "a committed revision row",
        ))
    });
    assert!(result.is_err());
    created.get().unwrap()
}

#[test]
fn batched_revision_payloads_reject_uncommitted_rows_without_losing_id_alignment() {
    let store = GraphStore::new(GrafeoDB::new_in_memory());
    let committed = store.write(revision_metadata_node).unwrap();
    let rolled_back = rolled_back_metadata_node(&store);
    store
        .stable_read(|graph| {
            let rows = revision_rows_from_nodes(graph, &[committed])?;
            assert!(
                matches!(rows.as_slice(), [RevisionRow::Project(lineage)] if lineage == "lineage")
            );
            for ids in [[committed, rolled_back], [rolled_back, committed]] {
                let error = revision_rows_from_nodes(graph, &ids).unwrap_err();
                assert!(error.to_string().contains(&rolled_back.to_string()));
            }
            assert!(revision_row_from_node(graph, rolled_back).is_err());
            Ok(())
        })
        .unwrap();
}
