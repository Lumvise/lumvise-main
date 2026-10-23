//! Write-maintained `(project_root, path)` element index. The first path read builds it;
//! every committed element mutation then updates it under the writer gate, so a prefix read
//! selects its elements without scanning the project and never lags the stable graph.

use super::graph_rows::value_string;
use crate::domain::graph_views::normalized_path;
use grafeo::{GrafeoDB, NodeId};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Default)]
pub(super) struct ElementPathIndex {
    built: bool,
    keys: BTreeSet<(String, String, u64)>,
    locations: BTreeSet<(String, String, u64)>,
    by_node: HashMap<u64, (String, String)>,
}

impl ElementPathIndex {
    pub(super) fn is_built(&self) -> bool {
        self.built
    }

    /// Requires the caller's graph lease so the rebuilt index matches the readable graph.
    pub(super) fn rebuild(&mut self, graph: &GrafeoDB) {
        *self = Self {
            built: true,
            ..Self::default()
        };
        self.update(graph, graph.graph_store().nodes_by_label("SemanticElement"));
    }

    /// Requires the writer gate: `touched` were created or changed and `deleted` removed by one commit.
    pub(super) fn apply(
        &mut self,
        graph: &GrafeoDB,
        touched: &HashSet<u64>,
        deleted: &HashSet<u64>,
    ) {
        if !self.built {
            return;
        }
        for id in deleted {
            self.remove(*id);
        }
        let live = touched.difference(deleted).map(|id| NodeId(*id)).collect();
        self.update(graph, live);
    }

    pub(super) fn select(&self, project_root: &str, prefix: &str) -> Vec<NodeId> {
        self.keys
            .range((project_root.to_owned(), prefix.to_owned(), 0)..)
            .take_while(|(root, path, _)| root == project_root && path.starts_with(prefix))
            .map(|(_, _, id)| NodeId(*id))
            .collect()
    }

    pub(super) fn select_location(&self, root: &str, path: &str) -> Vec<NodeId> {
        self.locations
            .range(
                (root.to_owned(), path.to_owned(), 0)
                    ..=(root.to_owned(), path.to_owned(), u64::MAX),
            )
            .map(|(_, _, id)| NodeId(*id))
            .collect()
    }

    fn update(&mut self, graph: &GrafeoDB, ids: Vec<NodeId>) {
        let keys = ["project_root", "path"].map(Into::into);
        let rows = graph
            .graph_store()
            .get_nodes_properties_selective_batch(&ids, &keys);
        for (id, row) in ids.into_iter().zip(rows) {
            self.remove(id.0);
            if let (Some(root), Some(path)) = (
                row.get("project_root").and_then(value_string),
                row.get("path").and_then(value_string),
            ) {
                self.keys.insert((root.clone(), path.clone(), id.0));
                self.locations
                    .insert((root.clone(), normalized_path(&path).to_owned(), id.0));
                self.by_node.insert(id.0, (root, path));
            }
        }
    }

    fn remove(&mut self, id: u64) {
        if let Some((root, path)) = self.by_node.remove(&id) {
            self.locations
                .remove(&(root.clone(), normalized_path(&path).to_owned(), id));
            self.keys.remove(&(root, path, id));
        }
    }
}
