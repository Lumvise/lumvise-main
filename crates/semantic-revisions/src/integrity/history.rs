//! Recovery audit validation belongs to archive assembly, never to ordinary read availability.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionState {
    /// Checks a complete unavailable-header/audit catalog without requiring available bodies or selections.
    /// Example: `catalog.validate_integrity_history()?` before acknowledging another loss.
    pub fn validate_integrity_history(&self) -> SemanticRevisionResult<()> {
        validate_recovery_history(self)
    }
}

pub(crate) fn validate_recovery_history(
    state: &SemanticRevisionState,
) -> SemanticRevisionResult<()> {
    crate::revision_index::require_project_identity(&state.project_id)?;
    let records = state
        .records
        .iter()
        .map(|stored| (&stored.record, stored))
        .collect::<BTreeMap<_, _>>();
    for accepted in &state.recovery_operations {
        validate_receipt(state, accepted, &records)?;
    }
    for stored in &state.records {
        let SemanticRevisionBody::Unavailable(marker) = &stored.body else {
            continue;
        };
        let acknowledged = state.recovery_operations.iter().any(|accepted| {
            accepted.receipt.operation_id == marker.recovery_operation_id
                && accepted.receipt.unavailable_at_ms == marker.unavailable_at_ms
                && accepted
                    .receipt
                    .unavailable_records
                    .contains(&stored.record)
        });
        if !acknowledged {
            return Err(missing_audit(&stored.record.record_id));
        }
    }
    Ok(())
}

fn validate_receipt(
    state: &SemanticRevisionState,
    accepted: &SemanticRevisionAcceptedRecovery,
    records: &BTreeMap<&SemanticRecordRef, &SemanticStoredRevision>,
) -> SemanticRevisionResult<()> {
    let receipt = &accepted.receipt;
    let unique = receipt.unavailable_records.iter().collect::<BTreeSet<_>>();
    if receipt.operation_id.trim().is_empty()
        || accepted.request_root.trim().is_empty()
        || accepted.request_digest.trim().is_empty()
        || receipt.project_id != state.project_id
        || receipt.previous_commit_version < 0
        || receipt.commit_version <= receipt.previous_commit_version
        || receipt.commit_version > state.commit_version
        || unique.is_empty()
        || unique.len() != receipt.unavailable_records.len()
    {
        return Err(missing_audit(&receipt.operation_id));
    }
    for reference in &receipt.unavailable_records {
        validate_audited_record(receipt, reference, records.get(reference).copied())?;
    }
    Ok(())
}

fn validate_audited_record(
    receipt: &SemanticRevisionRecoveryReceipt,
    reference: &SemanticRecordRef,
    stored: Option<&SemanticStoredRevision>,
) -> SemanticRevisionResult<()> {
    let Some(SemanticStoredRevision {
        body: SemanticRevisionBody::Unavailable(marker),
        ..
    }) = stored
    else {
        return Err(missing_audit(&reference.record_id));
    };
    if reference.kind != SemanticRecordKind::Element
        || marker.recovery_operation_id != receipt.operation_id
        || marker.unavailable_at_ms != receipt.unavailable_at_ms
    {
        return Err(missing_audit(&reference.record_id));
    }
    Ok(())
}

fn missing_audit(value: &str) -> SemanticRevisionError {
    SemanticRevisionError::invalid_value(
        value,
        "retained unavailable element metadata matching its exact project recovery audit",
    )
}
