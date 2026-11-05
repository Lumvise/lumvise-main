//! Visibility adapter for column reads. Native readers may call this trait;
//! raw Grafeo columns remain internal because rollback can leave indexed properties.
use super::graph_reads::GrafeoResult;
use crate::Result;
use grafeo::{EdgeId, NodeId, Snapshot};
/// Preserve ID alignment: `rows.committed_nodes(snapshot, ids)?` hides uncommitted rows.
pub(super) trait CommittedPropertyRows: Sized {
    fn committed_nodes(self, snapshot: &Snapshot, ids: &[NodeId]) -> Result<Self>;
    fn committed_edges(self, snapshot: &Snapshot, ids: &[EdgeId]) -> Result<Self>;
}

impl<T: Default> CommittedPropertyRows for Vec<T> {
    fn committed_nodes(self, snapshot: &Snapshot, ids: &[NodeId]) -> Result<Self> {
        // GRAFEO06-REVIEW(1): 0.6 batch reads already take a ReadView, so this mask repeats it.
        let view = snapshot.view();
        mask_uncommitted_rows(self, ids, |id| snapshot.is_node_visible(*id, view).db())
    }

    fn committed_edges(self, snapshot: &Snapshot, ids: &[EdgeId]) -> Result<Self> {
        // GRAFEO06-REVIEW(2): same repeat for edge columns.
        let view = snapshot.view();
        mask_uncommitted_rows(self, ids, |id| snapshot.is_edge_visible(*id, view).db())
    }
}

fn mask_uncommitted_rows<T: Default, I>(
    mut rows: Vec<T>,
    ids: &[I],
    visible: impl Fn(&I) -> Result<bool>,
) -> Result<Vec<T>> {
    for (row, id) in rows.iter_mut().zip(ids) {
        if !visible(id)? {
            *row = T::default();
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests;
