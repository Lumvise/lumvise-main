//! Write-maintained `(project_root, path)` element index. The first path read builds it;
//! every committed element mutation then updates it under the writer gate, so a prefix read
//! selects its elements without scanning the project and never lags the stable graph.

use super::graph_rows::value_string;
use crate::Result;
use crate::domain::graph_views::normalized_path;
use crate::local::grafeo::committed_columns::CommittedPropertyRows;
use crate::local::grafeo::graph_reads::{CommittedGraph, GrafeoResult};
use grafeo::{GrafeoDB, NodeId, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

#[cfg(test)]
mod tests;

pub(super) struct ElementPathIndex {
    label: &'static str,
    root_property: &'static str,
    path_property: &'static str,
    current_only: bool,
    built: bool,
    keys: BTreeSet<(String, String, u64)>,
    locations: BTreeSet<(String, String, u64)>,
    by_node: HashMap<u64, (String, String)>,
}

impl Default for ElementPathIndex {
    fn default() -> Self {
        Self {
            label: "SemanticElement",
            root_property: "project_root",
            path_property: "path",
            current_only: true,
            built: false,
            keys: BTreeSet::new(),
            locations: BTreeSet::new(),
            by_node: HashMap::new(),
        }
    }
}

impl ElementPathIndex {
    pub(super) fn revision_headers() -> Self {
        Self {
            label: "SemanticRevisionRow",
            root_property: "revision_project_root",
            path_property: "revision_record_path",
            current_only: false,
            ..Self::default()
        }
    }
    pub(super) fn is_built(&self) -> bool {
        self.built
    }

    /// Requires the caller's graph lease so the rebuilt index matches the readable graph.
    pub(super) fn rebuild(&mut self, graph: &GrafeoDB) -> Result<()> {
        *self = Self {
            built: true,
            label: self.label,
            root_property: self.root_property,
            path_property: self.path_property,
            current_only: self.current_only,
            ..Self::default()
        };
        let snapshot = graph.committed_snapshot()?;
        let ids = snapshot.nodes_by_label(self.label, snapshot.view()).db()?;
        let updated = self.update(graph, ids);
        if updated.is_err() {
            self.built = false;
        }
        updated
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
        // The commit already happened; a failed refresh forces the next read to rebuild.
        if self.update(graph, live).is_err() {
            self.built = false;
        }
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

    fn update(&mut self, graph: &GrafeoDB, ids: Vec<NodeId>) -> Result<()> {
        let keys = [self.root_property, self.path_property, "revision_selected"].map(Into::into);
        // GRAFEO06-REVIEW(21): runs after commit under the writer gate; committed view must include that commit.
        let snapshot = graph.committed_snapshot()?;
        let rows = snapshot
            .node_properties_selective_batch(&ids, &keys, snapshot.view())
            .db()?
            .committed_nodes(&snapshot, &ids)?;
        for (id, row) in ids.into_iter().zip(rows) {
            self.remove(id.0);
            if self.current_only && row.get("revision_selected") == Some(&Value::from(false)) {
                continue;
            }
            if let (Some(root), Some(path)) = (
                row.get(self.root_property).and_then(value_string),
                row.get(self.path_property).and_then(value_string),
            ) {
                self.keys.insert((root.clone(), path.clone(), id.0));
                self.locations
                    .insert((root.clone(), normalized_path(&path).to_owned(), id.0));
                self.by_node.insert(id.0, (root, path));
            }
        }
        Ok(())
    }

    fn remove(&mut self, id: u64) {
        if let Some((root, path)) = self.by_node.remove(&id) {
            self.locations
                .remove(&(root.clone(), normalized_path(&path).to_owned(), id));
            self.keys.remove(&(root, path, id));
        }
    }
}
