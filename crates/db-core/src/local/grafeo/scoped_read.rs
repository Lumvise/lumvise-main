//! Grafeo snapshot adapter; scoped traversal belongs to the shared domain.
mod roots;
use super::{
    graph_row_projection::semantic_relationship_from_edge,
    graph_rows::{elements_by_ids_selective, semantic_element_by_id_selective},
    graph_store::GraphStore,
    semantic_storage::SemanticStorage,
};
use crate::domain::graph_views::{GraphViewSource, scoped_from_source};
use crate::{
    Result, ScopedSemanticRead, SemanticDependencyDirection, SemanticElement, SemanticRelationship,
    SemanticScopedGraph,
};
use grafeo::{GrafeoDB, Value};
use grafeo_core::graph::Direction;
use std::collections::HashSet;
struct GrafeoViewSource<'a> {
    graph: &'a GrafeoDB,
    store: &'a GraphStore,
}
impl SemanticStorage<'_> {
    pub(crate) fn scoped_read(&self, request: &ScopedSemanticRead) -> Result<SemanticScopedGraph> {
        self.graph.stable_read(|graph| {
            let (revision, published_at) = self.latest_graph_publication()?;
            scoped_from_source(
                &GrafeoViewSource {
                    graph,
                    store: self.graph,
                },
                request,
                revision,
                published_at,
            )
        })
    }
}
impl GraphViewSource for GrafeoViewSource<'_> {
    fn validate_root(&self, root: &str) -> Result<()> {
        crate::local::sql::validation::require_non_empty(root, "non-empty project root")
    }
    fn element(&self, id: &str) -> Option<SemanticElement> {
        semantic_element_by_id_selective(self.graph, id)
    }
    fn elements(&self, root: &str, ids: &HashSet<String>, inactive: bool) -> Vec<SemanticElement> {
        elements_by_ids_selective(self.graph, root, ids, inactive)
    }
    fn roots(&self, root: &str, inactive: bool) -> Vec<String> {
        roots::project_roots(self.graph, root, inactive)
    }
    fn scope_count(&self, root: &str, inactive: bool) -> usize {
        self.store.scope_element_count(self.graph, root, inactive)
    }
    fn location(
        &self,
        root: &str,
        path: Option<&str>,
        line: i64,
        inactive: bool,
    ) -> Vec<SemanticElement> {
        self.store
            .location_elements(self.graph, root, path, line, inactive)
    }
    fn edges(
        &self,
        root: &str,
        id: &str,
        direction: SemanticDependencyDirection,
    ) -> Vec<SemanticRelationship> {
        let direction = match direction {
            SemanticDependencyDirection::Dependencies => Direction::Outgoing,
            SemanticDependencyDirection::Dependents => Direction::Incoming,
            SemanticDependencyDirection::Both => Direction::Both,
        };
        self.graph
            .find_nodes_by_property("semantic_element_id", &Value::from(id))
            .into_iter()
            .flat_map(|node| {
                self.graph
                    .store()
                    .edges_from(node, direction)
                    .collect::<Vec<_>>()
            })
            .filter_map(|(_, edge)| self.graph.get_edge(edge))
            .filter_map(|edge| semantic_relationship_from_edge(&edge))
            .filter(|edge| edge.project_root == root)
            .collect()
    }
}
