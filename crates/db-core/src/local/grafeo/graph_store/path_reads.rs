//! Stable path reads and post-commit index maintenance share the graph publication lease.
use super::*;

impl GraphState {
    /// Requires the caller's stable graph lease; selected IDs and hydration
    /// must observe the same revision as the surrounding scoped result.
    pub(in crate::local::grafeo) fn location_elements(
        &self,
        graph: &GrafeoDB,
        project_root: &str,
        path: Option<&str>,
        line: i64,
        include_inactive: bool,
    ) -> Vec<SemanticElement> {
        let Some(path) = path else {
            return Vec::new();
        };
        let mut index = self
            .path_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !index.is_built() {
            index.rebuild(graph);
        }
        let ids = index.select_location(project_root, path);
        drop(index);
        super::super::graph_rows::selective_current_elements(graph, &ids, Some(project_root), true)
            .into_iter()
            .filter(|element| {
                (include_inactive || element.lifecycle == "active")
                    && crate::domain::graph_views::element_contains_line(element, line)
            })
            .collect()
    }

    /// Commits, then folds the committed element changes into the path index before the
    /// writer gate is released; a rolled-back transaction leaves the index untouched.
    pub(super) fn finish_indexed<R>(
        &self,
        transaction: GraphTransaction<'_>,
        write_result: Result<R>,
    ) -> Result<R> {
        let touched = transaction.path_mutations.take();
        let deleted = transaction.path_deletions.take();
        let result = transaction.finish(write_result);
        if result.is_ok() {
            for index in [&self.path_index, &self.revision_path_index] {
                index
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .apply(&self.database, &touched, &deleted);
            }
        }
        result
    }

    /// Requires the caller's stable graph lease: writers cannot change the graph or the
    /// index while this selects, so the ids and their hydration share one revision.
    pub(in crate::local::grafeo) fn path_prefix_elements(
        &self,
        graph: &GrafeoDB,
        project_root: &str,
        prefix: &str,
        include_inactive: bool,
    ) -> Vec<SemanticElement> {
        let mut index = self
            .path_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !index.is_built() {
            index.rebuild(graph);
        }
        let ids = index.select(project_root, prefix);
        drop(index);
        super::super::graph_rows::selective_current_elements(
            graph,
            &ids,
            Some(project_root),
            include_inactive,
        )
    }

    pub(in crate::local::grafeo) fn path_prefix_revision_headers(
        &self,
        graph: &GrafeoDB,
        root: &str,
        prefix: &str,
    ) -> Vec<NodeId> {
        let mut index = self
            .revision_path_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !index.is_built() {
            index.rebuild(graph);
        }
        index.select(root, prefix)
    }
}
