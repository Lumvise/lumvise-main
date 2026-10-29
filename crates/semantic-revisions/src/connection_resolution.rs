//! Exact connection metadata lets a checkout acknowledge opaque IDs without rereading a moving head.
use crate::revision_index::{ensure_root, require_project_identity, unique_nonempty};
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Exact immutable connection IDs in an observed project lineage, independent of current selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionConnectionQuery {
    pub project_root: String,
    pub project_id: String,
    pub connection_ids: Vec<String>,
}

impl SemanticRevisionState {
    /// Resolves retained definitions in request order, even after endpoint bodies are collected.
    /// Example: `state.resolve_connections(&query)?` rejects a missing ID rather than guessing.
    pub fn resolve_connections(
        &self,
        query: &SemanticRevisionConnectionQuery,
    ) -> SemanticRevisionResult<Vec<SemanticRevisionConnection>> {
        validate_connection_query(self, query)?;
        let index = connection_definitions(self)?;
        query
            .connection_ids
            .iter()
            .map(|id| {
                index
                    .get(id.as_str())
                    .map(|edge| (*edge).clone())
                    .ok_or_else(|| {
                        SemanticRevisionError::invalid_value(
                            id,
                            "an exact retained connection definition",
                        )
                    })
            })
            .collect()
    }
}

fn validate_connection_query(
    state: &SemanticRevisionState,
    query: &SemanticRevisionConnectionQuery,
) -> SemanticRevisionResult<()> {
    ensure_root(&query.project_root, &state.project_root)?;
    require_project_identity(&query.project_id)?;
    if query.project_id != state.project_id {
        return Err(SemanticRevisionError::invalid_value(
            &query.project_id,
            format!("observed project lineage `{}`", state.project_id),
        ));
    }
    unique_nonempty(
        &query.connection_ids,
        "unique non-empty exact connection IDs",
    )
}

fn connection_definitions(
    state: &SemanticRevisionState,
) -> SemanticRevisionResult<BTreeMap<&str, &SemanticRevisionConnection>> {
    let mut index = BTreeMap::new();
    for edge in &state.connections {
        ensure_root(&edge.relationship.project_root, &state.project_root)?;
        if edge.connection_id.trim().is_empty()
            || index.insert(edge.connection_id.as_str(), edge).is_some()
        {
            return Err(SemanticRevisionError::invalid_value(
                &edge.connection_id,
                "one retained definition per non-empty connection ID",
            ));
        }
    }
    Ok(index)
}
