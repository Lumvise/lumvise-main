//! Visibility adapter for column reads. Native readers may call this trait;
//! raw Grafeo columns remain internal because rollback can leave indexed properties.
use grafeo::{EdgeId, NodeId};
use grafeo_core::graph::GraphStore;
/// Preserve ID alignment: `rows.committed_nodes(store, ids)` hides uncommitted rows.
pub(super) trait CommittedPropertyRows: Sized {
    fn committed_nodes(self, store: &dyn GraphStore, ids: &[NodeId]) -> Self;
    fn committed_edges(self, store: &dyn GraphStore, ids: &[EdgeId]) -> Self;
}

impl<T: Default> CommittedPropertyRows for Vec<T> {
    fn committed_nodes(self, store: &dyn GraphStore, ids: &[NodeId]) -> Self {
        let epoch = store.current_epoch();
        mask_uncommitted_rows(self, ids, |id| store.is_node_visible_at_epoch(*id, epoch))
    }

    fn committed_edges(self, store: &dyn GraphStore, ids: &[EdgeId]) -> Self {
        let epoch = store.current_epoch();
        mask_uncommitted_rows(self, ids, |id| store.is_edge_visible_at_epoch(*id, epoch))
    }
}

fn mask_uncommitted_rows<T: Default, I>(
    mut rows: Vec<T>,
    ids: &[I],
    visible: impl Fn(&I) -> bool,
) -> Vec<T> {
    for (row, id) in rows.iter_mut().zip(ids) {
        if !visible(id) {
            *row = T::default();
        }
    }
    rows
}

#[cfg(test)]
mod tests;
