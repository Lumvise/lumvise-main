//! Best-effort graph reads for search. Publication, exports and revision bases
//! keep their stable leases; these records never authorize a write.
use super::graph_reads::GrafeoResult;
use super::graph_rows::{selective_current_elements, semantic_relationships_for_project_edges};
use super::semantic_storage::SemanticStorage;
use crate::{Result, SemanticElement, SemanticProjectSnapshot, SemanticRelationship};
use grafeo::{GrafeoDB, Value};
use std::collections::HashSet;

impl SemanticStorage<'_> {
    pub(crate) fn search_project_structure(
        &self,
        project_root: &str,
    ) -> Result<SemanticProjectSnapshot> {
        crate::local::sql::validation::require_non_empty(project_root, "non-empty project root")?;
        self.graph.read(|graph| {
            let (elements, relationships) = live_search_records(graph, project_root)?;
            let (commit_version, published_at) = self.graph.published_graph_watermark()?;
            Ok(SemanticProjectSnapshot {
                commit_version,
                published_at,
                project_root: project_root.into(),
                elements,
                relationships,
                artifacts: Vec::new(),
            })
        })
    }
}

fn live_search_records(
    graph: &GrafeoDB,
    project_root: &str,
) -> Result<(Vec<SemanticElement>, Vec<SemanticRelationship>)> {
    let ids = graph
        .find_nodes_by_property("project_root", &Value::from(project_root))
        .db()?;
    let elements = selective_current_elements(graph, &ids, Some(project_root), true)?;
    let relationships = live_search_edges(graph, project_root, &elements)?;
    Ok((elements, relationships))
}

fn live_search_edges(
    graph: &GrafeoDB,
    project_root: &str,
    elements: &[SemanticElement],
) -> Result<Vec<SemanticRelationship>> {
    let owners = elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<HashSet<_>>();
    Ok(
        semantic_relationships_for_project_edges(graph, project_root)?
            .into_iter()
            .filter(|edge| {
                owners.contains(edge.source_element_id.as_str())
                    && owners.contains(edge.target_element_id.as_str())
            })
            .collect(),
    )
}
