//! Atomic local activation of shared-policy archive reconciliation through the existing publication owner.
use super::graph_rows::{self, RevisionProjection, VectorReplacePlan};
use super::revision_rows::PreparedRevisionRows;
use super::semantic_storage::SemanticStorage;
use crate::local::sql::{artifact_blobs::put_archived_blob, commits::begin_semantic_commit_after};
use crate::*;
use grafeo::GrafeoDB;
use std::collections::{BTreeMap, BTreeSet};

struct PreparedArchiveRestore {
    empty_project: bool,
    rows: PreparedRevisionRows,
    projection: RevisionProjection,
    element_vectors: Vec<VectorReplacePlan>,
    artifact_vectors: Vec<VectorReplacePlan>,
    result: PzImportResult,
}

impl SemanticStorage<'_> {
    pub(crate) fn restore_semantic_archive(
        &self,
        archive: &SemanticArchive,
    ) -> Result<PzImportResult> {
        let incoming = archive.revision_state.as_ref().ok_or_else(|| {
            DbError::invalid_value(
                &archive.snapshot_id,
                "complete revision metadata for archive activation",
            )
        })?;
        if archive.selection_id != "working" {
            return Err(DbError::invalid_value(
                &archive.selection_id,
                "the working selection for local archive activation",
            ));
        }
        let now = self.clock.now().timestamp_millis();
        let prepare =
            |graph: &GrafeoDB| self.prepare_archive_restore(graph, archive, incoming, now);
        let initial = self.graph.plan(&prepare)?;
        self.commit_prepared_graph_write_with_sql_start(
            initial,
            prepare,
            |transaction, prepared| {
                let version = begin_semantic_commit_after(transaction, incoming.commit_version)?;
                crate::local::sql::project_lineages::restore_project_identity(
                    transaction,
                    &incoming.project_root,
                    &archive.project_id,
                    prepared.empty_project,
                )?;
                for blob in &archive.artifact_blobs {
                    put_archived_blob(transaction, blob)?;
                }
                Ok(version)
            },
            |graph, version, collector, prepared| {
                prepared.projection.apply(graph, version, collector)?;
                prepared.rows.apply(graph, version)?;
                for vector in prepared.element_vectors {
                    graph_rows::apply_element_name_vector_replace(graph, vector)?;
                }
                for vector in prepared.artifact_vectors {
                    graph_rows::apply_artifact_vector_replace(graph, vector)?;
                }
                Ok(prepared.result)
            },
            |_, _| Ok(()),
        )
    }

    fn prepare_archive_restore(
        &self,
        graph: &GrafeoDB,
        archive: &SemanticArchive,
        incoming: &SemanticRevisionState,
        now: i64,
    ) -> Result<PreparedArchiveRestore> {
        let before = self.load_revisions(graph, &incoming.project_root)?;
        let after = before.state.merge_archive(incoming, now)?;
        let (element_vectors, artifact_vectors) = prepare_archive_vectors(graph, archive, &after)?;
        Ok(PreparedArchiveRestore {
            empty_project: before.state.records.is_empty(),
            rows: before.prepare_archive_restore(&after)?,
            projection: RevisionProjection::prepare_restore(
                graph,
                &before.state,
                &after,
                before.adopted,
            )?,
            result: archive_restore_result(archive, &before.state, &after),
            element_vectors,
            artifact_vectors,
        })
    }
}

fn prepare_archive_vectors(
    graph: &GrafeoDB,
    archive: &SemanticArchive,
    state: &SemanticRevisionState,
) -> Result<(Vec<VectorReplacePlan>, Vec<VectorReplacePlan>)> {
    let elements = state
        .records
        .iter()
        .filter_map(|row| match &row.body {
            SemanticRevisionBody::Element(body) => Some((&body.semantic_element_id, body)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let artifacts = state
        .records
        .iter()
        .filter_map(|row| match &row.body {
            SemanticRevisionBody::Artifact(body) => Some((&body.artifact_id, body)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let element_vectors = archive
        .element_name_vectors
        .iter()
        .map(|stored| {
            let body = elements.get(&stored.semantic_element_id).ok_or_else(|| {
                DbError::invalid_value(
                    &stored.semantic_element_id,
                    "an available exact element for its archived vector",
                )
            })?;
            graph_rows::prepare_element_name_vector_replace(
                graph,
                body,
                &stored.source_text,
                &stored.vector,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let artifact_vectors = archive
        .artifact_text_vectors
        .iter()
        .map(|stored| {
            let body = artifacts.get(&stored.artifact_id).ok_or_else(|| {
                DbError::invalid_value(
                    &stored.artifact_id,
                    "an available exact artifact for its archived vector",
                )
            })?;
            if body.semantic_element_id != stored.semantic_element_id {
                return Err(DbError::invalid_value(
                    &stored.semantic_element_id,
                    format!("the exact artifact owner {}", body.semantic_element_id),
                ));
            }
            graph_rows::prepare_artifact_vector_replace(
                graph,
                body,
                &stored.source_text,
                &stored.vector,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((element_vectors, artifact_vectors))
}

fn archive_restore_result(
    archive: &SemanticArchive,
    before: &SemanticRevisionState,
    after: &SemanticRevisionState,
) -> PzImportResult {
    let available = before
        .records
        .iter()
        .filter(|row| !matches!(row.body, SemanticRevisionBody::Pruned))
        .map(|row| &row.record)
        .collect::<BTreeSet<_>>();
    let incoming = archive
        .revision_state
        .as_ref()
        .expect("validated archive metadata");
    let new = incoming
        .records
        .iter()
        .filter(|row| {
            !matches!(row.body, SemanticRevisionBody::Pruned) && !available.contains(&row.record)
        })
        .collect::<Vec<_>>();
    let artifacts_imported = new
        .iter()
        .filter(|row| row.record.kind == SemanticRecordKind::Artifact)
        .count();
    let artifact_count = incoming
        .records
        .iter()
        .filter(|row| matches!(row.body, SemanticRevisionBody::Artifact(_)))
        .count();
    let selected = after
        .selection("working")
        .expect("validated current selection");
    let old = before
        .selection("working")
        .into_iter()
        .flat_map(|head| head.records.iter())
        .collect::<BTreeSet<_>>();
    let current = selected.records.iter().collect::<BTreeSet<_>>();
    PzImportResult {
        project_id: archive.project_id.clone(),
        snapshot_id: archive.snapshot_id.clone(),
        canonical_remote: archive.canonical_remote.clone(),
        artifacts_imported,
        artifacts_kept: artifact_count - artifacts_imported,
        structure: SemanticBatchSyncReport {
            elements_upserted: new
                .iter()
                .filter(|row| row.record.kind == SemanticRecordKind::Element)
                .count(),
            relationships_upserted: selected.connection_ids.len(),
            identities_reused: incoming.records.len() - new.len(),
            elements_marked_inactive: old
                .difference(&current)
                .filter(|row| row.kind == SemanticRecordKind::Element)
                .count(),
        },
    }
}
