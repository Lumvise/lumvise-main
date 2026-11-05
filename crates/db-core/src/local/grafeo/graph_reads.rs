//! Committed graph reads for Grafeo 0.6, where every store read takes a
//! `ReadView` and returns a `Result`. `committed_snapshot` is the one place
//! that pins a committed view; reads that need a transaction's own writes
//! use the transaction's `Session` instead.
use crate::{DbError, Result};
use grafeo::store::{Edge, Node};
use grafeo::{Direction, GrafeoDB, Snapshot};

/// Maps a Grafeo failure to the same `DbError::Grafeo` the write path reports.
pub(super) trait GrafeoResult<T> {
    fn db(self) -> Result<T>;
}

impl<T> GrafeoResult<T> for grafeo::Result<T> {
    fn db(self) -> Result<T> {
        self.map_err(|error| DbError::Grafeo(error.to_string()))
    }
}

// GRAFEO06-REVIEW(34): every `GrafeoDB::find_nodes_by_property/get_node/get_edge/get_node_edges`
// call in the prepare_*/read paths is the 0.6 direct API, a committed reader at the current epoch.
// See GRAFEO06-READS.md "Direct API reads".
pub(super) trait CommittedGraph {
    /// The committed state at the current epoch, pinned while the snapshot lives.
    fn committed_snapshot(&self) -> Result<Snapshot>;
    /// Every committed node, by ascending id.
    fn all_nodes(&self) -> Result<Vec<Node>>;
    /// Every committed edge, each listed once from its source node.
    fn all_edges(&self) -> Result<Vec<Edge>>;
}

impl CommittedGraph for GrafeoDB {
    fn committed_snapshot(&self) -> Result<Snapshot> {
        self.snapshot().db()
    }

    fn all_nodes(&self) -> Result<Vec<Node>> {
        let snapshot = self.committed_snapshot()?;
        let view = snapshot.view();
        let mut nodes = Vec::new();
        for id in snapshot.node_ids(view).db()? {
            nodes.extend(snapshot.node(id, view).db()?);
        }
        Ok(nodes)
    }

    fn all_edges(&self) -> Result<Vec<Edge>> {
        let snapshot = self.committed_snapshot()?;
        let view = snapshot.view();
        let mut edges = Vec::new();
        for id in snapshot.node_ids(view).db()? {
            for (_, edge) in snapshot.edges(id, Direction::Outgoing, &[], view).db()? {
                edges.extend(snapshot.edge(edge, view).db()?);
            }
        }
        Ok(edges)
    }
}
