//! Metadata-only recovery stays inside the existing graph/SQL publication owner.
use super::*;
use crate::local::grafeo::revision_rows::{
    RevisionRow, inspect_revision_integrity, prepare_recovery_rows, revision_row_from_node,
    revision_row_nodes, unavailable_selection_headers,
};
use crate::local::sql::project_lineages::ProjectLineageRepository;

struct PreparedIntegrityRecovery {
    receipt: SemanticRevisionRecoveryReceipt,
    rows: Option<PreparedRevisionRows>,
}

impl SemanticStorage<'_> {
    pub(crate) fn unavailable_revisions(
        &self,
        root: &str,
        selection_id: &str,
    ) -> Result<Vec<SemanticRevisionRecordHeader>> {
        require_non_empty(root, "nonempty unavailable-revision project root")?;
        require_non_empty(selection_id, "nonempty unavailable-revision selection ID")?;
        self.graph
            .stable_read(|graph| unavailable_selection_headers(graph, root, selection_id))
    }

    pub(crate) fn inspect_revision_integrity(
        &self,
        root: &str,
        publication_operation_id: Option<&str>,
    ) -> Result<SemanticRevisionIntegrityReport> {
        require_non_empty(root, "nonempty integrity project root")?;
        self.graph.stable_read(|graph| {
            let mut report = self.inspect_integrity_at(graph, root)?;
            report.publication_inspection = publication_operation_id
                .map(|operation_id| inspect_publication_operation(graph, root, operation_id))
                .transpose()?;
            Ok(report)
        })
    }

    pub(crate) fn recover_revision_integrity(
        &self,
        request: &SemanticRevisionRecoveryRequest,
    ) -> Result<SemanticRevisionRecoveryReceipt> {
        let prepare = |graph: &GrafeoDB| self.prepare_integrity_recovery(graph, request);
        let initial = self.graph.plan(&prepare)?;
        if initial.value().rows.is_none() {
            return Ok(initial.into_parts().1.receipt);
        }
        self.commit_prepared_graph_write_with_sql_start(
            initial,
            prepare,
            |transaction, prepared| {
                if prepared.rows.is_none() {
                    return latest_published_commit_version(transaction);
                }
                begin_semantic_commit(transaction)
            },
            |_, _| Ok(()),
            |graph, commit, _collector, mut prepared| {
                if let Some(rows) = prepared.rows {
                    rows.apply(graph, commit)?;
                    prepared.receipt.commit_version = commit;
                }
                Ok(prepared.receipt)
            },
            |_, _| Ok(()),
        )
    }

    fn inspect_integrity_at(
        &self,
        graph: &GrafeoDB,
        root: &str,
    ) -> Result<SemanticRevisionIntegrityReport> {
        let identity = ProjectLineageRepository::new(self.conn).get_existing(root)?;
        let commit = latest_published_commit_version(&self.conn.read_conn())?;
        inspect_revision_integrity(graph, root, &identity.project_id, commit)
    }

    fn prepare_integrity_recovery(
        &self,
        graph: &GrafeoDB,
        request: &SemanticRevisionRecoveryRequest,
    ) -> Result<PreparedIntegrityRecovery> {
        let identity =
            ProjectLineageRepository::new(self.conn).get_existing(&request.project_root)?;
        if identity.project_id != request.project_id {
            return Err(DbError::invalid_value(
                &request.project_id,
                format!(
                    "the existing durable project identity {}",
                    identity.project_id
                ),
            ));
        }
        if let Some(receipt) = replay_integrity_recovery(graph, request)? {
            return Ok(PreparedIntegrityRecovery {
                receipt,
                rows: None,
            });
        }
        let report = self.inspect_integrity_at(graph, &request.project_root)?;
        let next = report.commit_version.checked_add(1).ok_or_else(|| {
            DbError::invalid_value(
                report.commit_version.to_string(),
                "an incrementable recovery commit",
            )
        })?;
        let plan = report.plan_recovery(request, self.clock.now().timestamp_millis(), next)?;
        let rows = prepare_recovery_rows(graph, &request.project_root, &plan)?;
        Ok(PreparedIntegrityRecovery {
            receipt: plan.accepted.receipt,
            rows: Some(rows),
        })
    }
}

fn inspect_publication_operation(
    graph: &GrafeoDB,
    root: &str,
    operation_id: &str,
) -> Result<SemanticRevisionPublicationInspection> {
    require_non_empty(
        operation_id,
        "a nonempty publication operation ID to inspect",
    )?;
    let nodes = revision_row_nodes(graph, root, "operation", operation_id)?;
    let accepted = match nodes.as_slice() {
        [] => None,
        [node] => Some(inspected_accepted_operation(graph, *node, operation_id)?),
        _ => {
            return Err(DbError::invalid_value(
                operation_id,
                "one accepted publication audit row",
            ));
        }
    };
    Ok(SemanticRevisionPublicationInspection {
        operation_id: operation_id.into(),
        accepted,
    })
}

fn inspected_accepted_operation(
    graph: &GrafeoDB,
    node: grafeo::NodeId,
    operation_id: &str,
) -> Result<SemanticRevisionAcceptedOperation> {
    match revision_row_from_node(graph, node)? {
        RevisionRow::Operation(accepted) if accepted.receipt.operation_id == operation_id => {
            Ok(accepted)
        }
        other => Err(DbError::invalid_value(
            format!("{other:?}"),
            format!("accepted publication metadata for {operation_id:?}"),
        )),
    }
}

fn replay_integrity_recovery(
    graph: &GrafeoDB,
    request: &SemanticRevisionRecoveryRequest,
) -> Result<Option<SemanticRevisionRecoveryReceipt>> {
    for kind in ["operation", "retention"] {
        if !revision_row_nodes(graph, &request.project_root, kind, &request.operation_id)?
            .is_empty()
        {
            return Err(DbError::invalid_value(
                &request.operation_id,
                "a recovery operation ID unused by publication or collection",
            ));
        }
    }
    let nodes = revision_row_nodes(
        graph,
        &request.project_root,
        "recovery",
        &request.operation_id,
    )?;
    if nodes.is_empty() {
        return Ok(None);
    }
    let [node] = nodes.as_slice() else {
        return Err(DbError::invalid_value(
            &request.operation_id,
            "one accepted recovery audit row",
        ));
    };
    let RevisionRow::Recovery(accepted) = revision_row_from_node(graph, *node)? else {
        return Err(DbError::invalid_value(
            &request.operation_id,
            "the accepted recovery metadata variant",
        ));
    };
    Ok(Some(accepted.replay(request)?))
}
