//! Native adapter for the shared revision policy. Existing graph/SQL publication owns atomicity.
use super::graph_rows::RevisionProjection;
use super::revision_rows::{LoadedRevisionState, PreparedRevisionRows, load_revision_state};
use super::semantic_storage::SemanticStorage;
use crate::local::sql::commits::{begin_semantic_commit, latest_published_commit_version};
use crate::local::sql::validation::require_non_empty;
use crate::*;
use grafeo::GrafeoDB;

mod citations;
mod contents;

struct PreparedRevisionPublication {
    plan: SemanticRevisionPublicationPlan,
    rows: PreparedRevisionRows,
    projection: RevisionProjection,
    contents: Vec<ArtifactBlob>,
    released_contents: Vec<String>,
}

struct PreparedRevisionCollection {
    receipt: SemanticRevisionRetentionReceipt,
    rows: Option<PreparedRevisionRows>,
    deleted_nodes: Vec<grafeo::NodeId>,
    released_contents: Vec<String>,
}

impl<'db> SemanticStorage<'db> {
    pub(crate) fn publish_revisions(
        &self,
        request: &SemanticRevisionPublication,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        let timestamp = self.revision_clock();
        if let Some(replay) = self.replay_revision_publication(request, &timestamp)? {
            return Ok(replay);
        }
        let prepare = |graph: &GrafeoDB| {
            let commit_version = latest_published_commit_version(&self.conn.read_conn())?;
            let scoped = super::revision_basis::load_publication_basis(
                graph,
                self.graph,
                request,
                commit_version,
                timestamp.unix_ms,
            )?;
            let (loaded, plan) = if let Some(loaded) = scoped {
                let basis = SemanticRevisionPublicationBasis {
                    structure_coverage: lumvise_semantic_revisions::structure_coverage(request),
                    artifact_ids: lumvise_semantic_revisions::requested_artifact_ids(request),
                    facts: loaded.state.clone(),
                };
                let plan = basis.plan_publication(request, &timestamp, commit_version + 1)?;
                (loaded, plan)
            } else {
                let loaded = self.load_revisions(graph, &request.project_root)?;
                let plan = loaded.state.plan_publication(
                    request,
                    &timestamp,
                    loaded.state.commit_version + 1,
                )?;
                (loaded, plan)
            };
            let contents = contents::prepare_revision_contents(request, &plan, &timestamp.rfc3339)?;
            self.prepare_revision_publication(graph, loaded, plan, contents)
        };
        self.commit_revision_publication(prepare)
    }

    pub(crate) fn select_revisions(
        &self,
        request: &SemanticRevisionSelectionUpdate,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        self.commit_selection_plan(&request.project_root, |state, timestamp, commit| {
            state.plan_selection(request, timestamp, commit)
        })
    }

    fn commit_selection_plan(
        &self,
        root: &str,
        plan: impl Fn(
            &SemanticRevisionState,
            &SemanticRevisionTimestamp,
            i64,
        ) -> lumvise_semantic_revisions::SemanticRevisionResult<
            SemanticRevisionPublicationPlan,
        >,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        let timestamp = self.revision_clock();
        let prepare = |graph: &GrafeoDB| {
            let loaded = self.load_revisions(graph, root)?;
            let plan = plan(&loaded.state, &timestamp, loaded.state.commit_version + 1)?;
            self.prepare_revision_publication(graph, loaded, plan, vec![])
        };
        self.commit_revision_publication(prepare)
    }

    fn prepare_revision_publication(
        &self,
        graph: &GrafeoDB,
        loaded: LoadedRevisionState,
        plan: SemanticRevisionPublicationPlan,
        contents: Vec<ArtifactBlob>,
    ) -> Result<PreparedRevisionPublication> {
        let projection = RevisionProjection::prepare(graph, &loaded.state, &plan, loaded.adopted)?;
        let rows = loaded.prepare_publication(&plan)?;
        let released_contents = contents::released_revision_contents(graph, &loaded.state, &plan);
        Ok(PreparedRevisionPublication {
            plan,
            rows,
            projection,
            contents,
            released_contents,
        })
    }

    fn commit_revision_publication(
        &self,
        prepare: impl Fn(&GrafeoDB) -> Result<PreparedRevisionPublication>,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        let initial = self.graph.plan(&prepare)?;
        if initial.value().plan.accepted_operation.is_none() {
            return Ok(initial.into_parts().1.plan.outcome);
        }
        self.commit_prepared_graph_write_with_sql_start(
            initial,
            prepare,
            |transaction, prepared| {
                let version = begin_semantic_commit(transaction)?;
                contents::stage_revision_contents(
                    transaction,
                    &prepared.contents,
                    &prepared.plan.records,
                )?;
                Ok(version)
            },
            |graph, commit_version, collector, mut prepared| {
                retarget_publication(&mut prepared.plan, commit_version);
                prepared
                    .projection
                    .apply(graph, commit_version, collector)?;
                prepared.rows.apply(graph, commit_version)?;
                Ok((prepared.plan.outcome, prepared.released_contents))
            },
            |transaction, (_, references)| {
                contents::remove_revision_contents(transaction, references)
            },
        )
        .map(|(outcome, _)| outcome)
    }

    pub(crate) fn plan_revision_retention(
        &self,
        project_root: &str,
        operation_id: &str,
        policy: &SemanticRevisionRetentionPolicy,
    ) -> Result<SemanticRevisionRetentionPlan> {
        let state = self.revision_history(project_root)?;
        Ok(state.plan_retention(operation_id, policy, self.clock.now().timestamp_millis())?)
    }

    pub(crate) fn collect_revision_retention(
        &self,
        plan: &SemanticRevisionRetentionPlan,
        policy: &SemanticRevisionRetentionPolicy,
    ) -> Result<SemanticRevisionRetentionReceipt> {
        let now_ms = self.clock.now().timestamp_millis();
        let prepare =
            |graph: &GrafeoDB| self.prepare_revision_collection(graph, plan, policy, now_ms);
        let initial = self.graph.plan(&prepare)?;
        if initial.value().rows.is_none() {
            return Ok(initial.into_parts().1.receipt);
        }
        self.commit_prepared_graph_write_with_sql(
            initial,
            prepare,
            |graph, commit_version, _collector, mut prepared| {
                let Some(rows) = prepared.rows else {
                    return Ok((prepared.receipt, None));
                };
                prepared.receipt.commit_version = commit_version;
                for node in prepared.deleted_nodes {
                    graph.delete_node(node);
                }
                rows.apply(graph, commit_version)?;
                Ok((prepared.receipt, Some(prepared.released_contents)))
            },
            |transaction, (_, references)| {
                let Some(references) = references else {
                    return Ok(());
                };
                contents::remove_revision_contents(transaction, references)
            },
        )
        .map(|(receipt, _)| receipt)
    }

    fn prepare_revision_collection(
        &self,
        graph: &GrafeoDB,
        plan: &SemanticRevisionRetentionPlan,
        policy: &SemanticRevisionRetentionPolicy,
        now_ms: i64,
    ) -> Result<PreparedRevisionCollection> {
        let loaded = self.load_revisions(graph, &plan.project_root)?;
        let mut after = loaded.state.clone();
        let receipt =
            after.apply_retention(plan, policy, now_ms, loaded.state.commit_version + 1)?;
        let replayed = loaded
            .state
            .retention_operations
            .iter()
            .any(|accepted| accepted.receipt.operation_id == plan.operation_id);
        let rows = if replayed {
            None
        } else {
            Some(loaded.prepare_collection(&after, &receipt)?)
        };
        let deleted_nodes = if replayed {
            vec![]
        } else {
            super::graph_rows::prepare_revision_body_collection(
                graph,
                &plan.project_root,
                &receipt.pruned_records,
            )?
        };
        let mut released_contents =
            self.released_attachment_contents(graph, &loaded.state, &after)?;
        released_contents.extend(receipt.removed_blob_refs.iter().cloned());
        Ok(PreparedRevisionCollection {
            receipt,
            rows,
            deleted_nodes,
            released_contents,
        })
    }

    pub(super) fn load_revisions(
        &self,
        graph: &GrafeoDB,
        project_root: &str,
    ) -> Result<LoadedRevisionState> {
        require_non_empty(project_root, "non-empty revision project root")?;
        let identity =
            crate::local::sql::project_lineages::ProjectLineageRepository::new(self.conn)
                .get_or_create(project_root)?;
        let commit_version = latest_published_commit_version(&self.conn.read_conn())?;
        load_revision_state(
            graph,
            project_root,
            commit_version,
            self.clock.now().timestamp_millis(),
            Some(&identity.project_id),
        )
    }

    fn revision_clock(&self) -> SemanticRevisionTimestamp {
        let now = self.clock.now();
        SemanticRevisionTimestamp {
            unix_ms: now.timestamp_millis(),
            rfc3339: now.to_rfc3339(),
        }
    }

    fn replay_revision_publication(
        &self,
        request: &SemanticRevisionPublication,
        timestamp: &SemanticRevisionTimestamp,
    ) -> Result<Option<SemanticRevisionPublicationOutcome>> {
        self.graph.stable_read(|graph| {
            let mut nodes = super::revision_rows::revision_row_nodes(
                graph,
                &request.project_root,
                "operation",
                &request.operation_id,
            )?;
            nodes.extend(super::revision_rows::revision_row_nodes(
                graph,
                &request.project_root,
                "retention",
                &request.operation_id,
            )?);
            if nodes.is_empty() {
                return Ok(None);
            }
            nodes.extend(super::revision_rows::revision_row_nodes(
                graph,
                &request.project_root,
                "project",
                "",
            )?);
            let state = super::revision_rows::load_revision_nodes(
                graph,
                &request.project_root,
                0,
                0,
                nodes,
                None,
                std::collections::BTreeMap::new(),
            )?
            .state;
            Ok(Some(state.plan_publication(request, timestamp, 1)?.outcome))
        })
    }
}

fn retarget_publication(plan: &mut SemanticRevisionPublicationPlan, commit_version: i64) {
    if plan.accepted_operation.is_none() {
        return;
    }
    for link in &mut plan.revision_links {
        link.commit_version = commit_version;
    }
    for delta in &mut plan.selection_deltas {
        delta.commit_version = commit_version;
    }
    if let Some(operation) = &mut plan.accepted_operation {
        operation.receipt.commit_version = commit_version;
        for link in &mut operation.receipt.revision_links {
            link.commit_version = commit_version;
        }
        plan.outcome = SemanticRevisionPublicationOutcome::Accepted(operation.receipt.clone());
    }
}

#[cfg(test)]
mod tests;
