//! Owns project aggregation inside Grafeo; no runtime element tree is materialized.
use super::semantic_storage::SemanticStorage;
use crate::{DbError, Result, SemanticResult};
use grafeo::{GrafeoDB, Value};
use std::collections::BTreeMap;

impl SemanticStorage<'_> {
    pub(crate) fn project_element_counts(&self, project_root: &str) -> Result<SemanticResult> {
        crate::local::sql::validation::require_non_empty(project_root, "non-empty project root")?;
        self.graph.stable_read(|graph| {
            self.check_active()?;
            let (commit_version, published_at) = self.latest_graph_publication()?;
            let elements_by_kind = count_project_kinds(graph, project_root)?;
            self.check_active()?;
            Ok(SemanticResult::ProjectElementCounts {
                commit_version,
                published_at,
                total_elements: elements_by_kind.values().sum(),
                elements_by_kind,
            })
        })
    }
}

fn count_project_kinds(graph: &GrafeoDB, project_root: &str) -> Result<BTreeMap<String, usize>> {
    // Grafeo 0.5.42's GQL equality planner hydrates whole nodes for visibility checks.
    // The publication lease permits its indexed, selective column API without that cost.
    let ids = super::graph_rows::node_ids_by_label_and_property(
        graph,
        "SemanticElement",
        "project_root",
        project_root,
    );
    let kinds = graph
        .graph_store()
        .get_node_property_batch(&ids, &"element_kind".into());
    let mut counts = BTreeMap::new();
    for value in kinds.into_iter().flatten() {
        let Value::String(kind) = value else {
            return Err(DbError::invalid_value(
                format!("{value:?}"),
                "semantic element kind string",
            ));
        };
        *counts.entry(kind.to_string()).or_default() += 1;
    }
    Ok(counts)
}
