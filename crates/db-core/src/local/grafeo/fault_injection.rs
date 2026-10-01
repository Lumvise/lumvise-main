#[cfg(debug_assertions)]
use crate::local::grafeo::graph_rows::upsert_element_node;
#[cfg(debug_assertions)]
use crate::local::grafeo::semantic_storage::SemanticStorage;
#[cfg(debug_assertions)]
use crate::{DbError, Result, SemanticElement};

#[cfg(debug_assertions)]
impl<'db> SemanticStorage<'db> {
    pub fn upsert_element_with_failed_publish(&self, element: &SemanticElement) -> Result<()> {
        self.commit_graph_write(|graph, commit_version, _collector| {
            upsert_element_node(graph, element, commit_version)?;
            Err(DbError::Grafeo("forced graph failure".to_string()))
        })
    }
}
