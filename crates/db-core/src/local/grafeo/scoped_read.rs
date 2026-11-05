//! Grafeo snapshot adapter; scoped traversal belongs to the shared domain.
mod roots;
use super::{
    graph_row_projection::semantic_relationship_from_edge,
    graph_rows::{elements_by_ids_selective, semantic_element_by_id_selective},
    graph_store::GraphStore,
    semantic_storage::SemanticStorage,
};
use crate::domain::graph_views::{GraphViewSource, scoped_from_source};
use crate::local::grafeo::graph_reads::GrafeoResult;
use crate::{
    DbError, Result, ScopedSemanticRead, SemanticDependencyDirection, SemanticElement,
    SemanticRelationship, SemanticScopedGraph,
};
use grafeo::{GrafeoDB, Value};
use grafeo_core::graph::Direction;
use std::cell::RefCell;
use std::collections::HashSet;
struct GrafeoViewSource<'a> {
    graph: &'a GrafeoDB,
    store: &'a GraphStore,
    /// The domain view contract is infallible; the first failed read is kept
    /// here and returned once the traversal ends.
    failure: RefCell<Option<DbError>>,
}

impl GrafeoViewSource<'_> {
    fn attempt<T: Default>(&self, read: impl FnOnce() -> Result<T>) -> T {
        read().unwrap_or_else(|error| {
            self.failure.borrow_mut().get_or_insert(error);
            T::default()
        })
    }
}
impl SemanticStorage<'_> {
    pub(crate) fn scoped_read(&self, request: &ScopedSemanticRead) -> Result<SemanticScopedGraph> {
        self.graph.stable_read(|graph| {
            let (revision, published_at) = self.latest_graph_publication()?;
            let source = GrafeoViewSource {
                graph,
                store: self.graph,
                failure: RefCell::new(None),
            };
            let scoped = scoped_from_source(&source, request, revision, published_at);
            match source.failure.take() {
                Some(error) => Err(error),
                None => scoped,
            }
        })
    }
}
impl GraphViewSource for GrafeoViewSource<'_> {
    fn validate_root(&self, root: &str) -> Result<()> {
        crate::local::sql::validation::require_non_empty(root, "non-empty project root")
    }
    fn element(&self, id: &str) -> Option<SemanticElement> {
        self.attempt(|| semantic_element_by_id_selective(self.graph, id))
    }
    fn elements(&self, root: &str, ids: &HashSet<String>, inactive: bool) -> Vec<SemanticElement> {
        self.attempt(|| elements_by_ids_selective(self.graph, root, ids, inactive))
    }
    fn roots(&self, root: &str, inactive: bool) -> Vec<String> {
        self.attempt(|| roots::project_roots(self.graph, root, inactive))
    }
    fn scope_count(&self, root: &str, inactive: bool) -> usize {
        self.attempt(|| self.store.scope_element_count(self.graph, root, inactive))
    }
    fn location(
        &self,
        root: &str,
        path: Option<&str>,
        line: i64,
        inactive: bool,
    ) -> Vec<SemanticElement> {
        self.attempt(|| {
            self.store
                .location_elements(self.graph, root, path, line, inactive)
        })
    }
    fn path_prefix(&self, root: &str, prefix: &str, inactive: bool) -> Vec<SemanticElement> {
        self.attempt(|| {
            self.store
                .path_prefix_elements(self.graph, root, prefix, inactive)
        })
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
        self.attempt(|| {
            let mut relationships = Vec::new();
            for node in self
                .graph
                .find_nodes_by_property("semantic_element_id", &Value::from(id))
                .db()?
            {
                // GRAFEO06-REVIEW(22): committed adjacency; 0.5 store().edges_from was live.
                for (_, edge) in self.graph.get_node_edges(node, direction, &[]).db()? {
                    if let Some(edge) = self.graph.get_edge(edge).db()?
                        && let Some(relationship) = semantic_relationship_from_edge(&edge)
                        && relationship.project_root == root
                    {
                        relationships.push(relationship);
                    }
                }
            }
            Ok(relationships)
        })
    }
}
