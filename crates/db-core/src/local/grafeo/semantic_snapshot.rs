//! Consistent, owned project graph snapshots for cross-process projection work.

use grafeo::GrafeoDB;

use crate::interface::ProjectSnapshotScope;
use crate::local::grafeo::graph_rows::{complete_project_rows, semantic_element_by_id_selective};
use crate::{DbError, Result, SemanticProjectSnapshot};

pub(crate) fn project_snapshot(
    graph: &GrafeoDB,
    scope: &ProjectSnapshotScope,
    artifact_namespace: Option<&str>,
    commit_version: i64,
    published_at: String,
) -> Result<SemanticProjectSnapshot> {
    let project_root = match scope {
        ProjectSnapshotScope::ProjectRoot(root) => root.clone(),
        ProjectSnapshotScope::SemanticElement(id) => semantic_element_by_id_selective(graph, id)
            .map(|element| element.project_root)
            .ok_or_else(|| DbError::invalid_value(id, "existing semantic element"))?,
    };
    let mut rows = complete_project_rows(graph, &project_root);
    rows.elements
        .sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
    rows.relationships
        .sort_by(|left, right| relationship_key(left).cmp(&relationship_key(right)));
    rows.artifacts.retain(|artifact| {
        artifact.metadata["association_kind"] != "inherited"
            && artifact_namespace.is_none_or(|namespace| artifact.metadata[namespace].is_object())
    });
    rows.artifacts
        .sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    Ok(SemanticProjectSnapshot {
        commit_version,
        published_at,
        project_root,
        elements: rows.elements,
        relationships: rows.relationships,
        artifacts: rows.artifacts,
    })
}

fn relationship_key(relationship: &crate::SemanticRelationship) -> (&str, &str, &str, &str) {
    (
        &relationship.source_element_id,
        &relationship.target_element_id,
        &relationship.relationship_kind,
        &relationship.label,
    )
}
