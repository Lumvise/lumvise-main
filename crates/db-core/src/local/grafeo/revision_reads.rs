//! Exact, selected and historical reads use indexed revision facts rather than loading every retained body.
use super::semantic_storage::SemanticStorage;
use crate::local::sql::commits::{latest_published_commit_version, published_commit_timestamp};
use crate::local::sql::validation::require_non_empty;
use crate::*;
use std::collections::HashSet;

pub(super) fn exact_element_body(
    graph: &grafeo::GrafeoDB,
    id: &str,
) -> Result<Option<SemanticElement>> {
    let Some((_, stored)) =
        super::revision_rows::stored_revision_by_id(graph, &SemanticRecordRef::element(id))?
    else {
        return Ok(super::graph_rows::semantic_element_by_id(graph, id));
    };
    Ok(match stored.body {
        SemanticRevisionBody::Element(element) => Some(element),
        _ => None,
    })
}

pub(super) fn exact_project_elements(
    graph: &grafeo::GrafeoDB,
    root: &str,
    ids: &HashSet<String>,
) -> Result<Vec<SemanticElement>> {
    let mut records = super::graph_rows::elements_by_ids_selective(graph, root, ids, true)
        .into_iter()
        .map(|element| (element.semantic_element_id.clone(), element))
        .collect::<std::collections::BTreeMap<_, _>>();
    for id in ids {
        let Some((scope, stored)) =
            super::revision_rows::stored_revision_by_id(graph, &SemanticRecordRef::element(id))?
        else {
            continue;
        };
        records.remove(id);
        if scope == root
            && let SemanticRevisionBody::Element(element) = stored.body
        {
            records.insert(id.clone(), element);
        }
    }
    Ok(records.into_values().collect())
}

impl SemanticStorage<'_> {
    pub(crate) fn revision_connections(
        &self,
        query: &SemanticRevisionConnectionQuery,
    ) -> Result<Vec<SemanticRevisionConnection>> {
        self.graph.stable_read(|graph| {
            let state = super::revision_basis::load_connection_revision_state(graph, query)?;
            Ok(state.resolve_connections(query)?)
        })
    }

    pub(crate) fn revision_history(&self, project_root: &str) -> Result<SemanticRevisionState> {
        require_non_empty(project_root, "non-empty revision project root")?;
        self.graph
            .stable_read(|graph| Ok(self.load_revisions(graph, project_root)?.state))
    }

    pub(crate) fn revision_selection(
        &self,
        project_root: &str,
        selection_id: &str,
    ) -> Result<Option<SemanticRevisionSelectionSnapshot>> {
        require_non_empty(selection_id, "non-empty revision selection ID")?;
        self.graph.stable_read(|graph| {
            let commit_version = latest_published_commit_version(&self.conn.read_conn())?;
            let state = if super::revision_rows::revision_project_managed(graph, project_root)? {
                super::revision_basis::load_selected_revision_state(
                    graph,
                    project_root,
                    selection_id,
                    commit_version,
                )?
            } else {
                self.load_revisions(graph, project_root)?.state
            };
            let published_at = self.revision_timestamp(state.commit_version)?;
            let mut snapshot = state.snapshot(selection_id, &published_at)?;
            if let Some(snapshot) = &mut snapshot {
                self.hydrate_artifact_contents(snapshot.artifacts.iter_mut().chain(
                    snapshot.required_records.iter_mut().filter_map(
                        |record| match &mut record.body {
                            SemanticRevisionBody::Artifact(artifact) => Some(artifact),
                            _ => None,
                        },
                    ),
                ))?;
            }
            Ok(snapshot)
        })
    }

    pub(crate) fn resolve_revision(
        &self,
        project_root: &str,
        record: &SemanticRecordRef,
    ) -> Result<Option<SemanticRevisionResolution>> {
        require_non_empty(project_root, "non-empty revision project root")?;
        self.graph.stable_read(|graph| {
            let Some((root, mut stored)) =
                super::revision_rows::stored_revision_by_id(graph, record)?
            else {
                return Ok(None);
            };
            if root != project_root {
                return Ok(None);
            }
            let key = serde_json::to_string(&(project_root, record))?;
            let mut successors = vec![];
            for node in
                graph.find_nodes_by_property("revision_link_identity", &grafeo::Value::from(key))
            {
                let super::revision_rows::RevisionRow::Link(link) =
                    super::revision_rows::revision_row_from_node(graph, node)?
                else {
                    return Err(DbError::invalid_value(
                        &record.record_id,
                        "a stored successor link",
                    ));
                };
                successors.push(link);
            }
            if let SemanticRevisionBody::Artifact(artifact) = &mut stored.body {
                self.hydrate_artifact_contents(std::slice::from_mut(artifact))?;
            }
            Ok(Some(SemanticRevisionResolution {
                element_scope: stored.element_scope,
                record: stored.record,
                body: stored.body,
                artifact_state: stored.artifact_state,
                created_at_ms: stored.created_at_ms,
                pruned_at_ms: stored.pruned_at_ms,
                successors,
            }))
        })
    }

    fn revision_timestamp(&self, commit_version: i64) -> Result<String> {
        if commit_version == 0 {
            return Ok(self.clock.now().to_rfc3339());
        }
        published_commit_timestamp(&self.conn.read_conn(), commit_version)
    }

    pub(crate) fn revision_selection_members(
        &self,
        query: &SemanticRevisionSelectionQuery,
    ) -> Result<Option<SemanticRevisionSelectionObservation>> {
        require_non_empty(&query.project_root, "non-empty revision project root")?;
        self.graph.stable_read(|graph| {
            let commit = latest_published_commit_version(&self.conn.read_conn())?;
            let state =
                if super::revision_rows::revision_project_managed(graph, &query.project_root)? {
                    super::revision_basis::load_revision_membership_state(
                        graph, self.graph, query, commit,
                    )?
                } else {
                    self.load_revisions(graph, &query.project_root)?.state
                };
            Ok(state.selection_members(query)?)
        })
    }

    pub(crate) fn revision_selection_at(
        &self,
        root: &str,
        selection: &str,
        generation: i64,
    ) -> Result<Option<SemanticRevisionSelectionState>> {
        require_non_empty(root, "non-empty revision project root")?;
        self.graph.stable_read(|graph| {
            let commit = latest_published_commit_version(&self.conn.read_conn())?;
            let state = super::revision_basis::load_selection_history_state(
                graph, root, selection, commit,
            )?;
            Ok(state.selection_at_generation(selection, generation)?)
        })
    }

    pub(crate) fn revision_source_anchor(
        &self,
        root: &str,
        selection: &str,
        anchor: &SemanticSourceAnchor,
    ) -> Result<Option<SemanticRevisionSelectionState>> {
        require_non_empty(root, "non-empty revision project root")?;
        self.graph.stable_read(|graph| {
            let key = serde_json::to_string(&(root, selection, anchor))?;
            let nodes =
                graph.find_nodes_by_property("revision_source_anchor", &grafeo::Value::from(key));
            if nodes.is_empty() && !anchor.dirty {
                return Ok(None);
            }
            let commit = latest_published_commit_version(&self.conn.read_conn())?;
            let state = super::revision_basis::load_selection_history_state(
                graph, root, selection, commit,
            )?;
            Ok(state.selection_for_source_anchor(selection, anchor)?)
        })
    }
}
