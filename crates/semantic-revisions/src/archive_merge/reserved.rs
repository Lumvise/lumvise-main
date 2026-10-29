//! A general archive writer cannot acquire the authority of a citation or contribution facade.
use super::*;

pub(super) fn validate(
    current: &SemanticRevisionState,
    incoming: &SemanticRevisionState,
) -> SemanticRevisionResult<()> {
    if current.records.is_empty()
        && current.selection_history.is_empty()
        && current.accepted_operations.is_empty()
        && current.retention_operations.is_empty()
    {
        return Ok(());
    }
    for head in &incoming.selections {
        if (reserved(&head.selection.selection_id) || reserved(&head.selection.source_context))
            && current.selection(&head.selection.selection_id) != Some(head)
        {
            return Err(archive_conflict(
                &head.selection.selection_id,
                "an identical existing reserved selection",
            ));
        }
    }
    for delta in &incoming.selection_history {
        if (reserved(&delta.selection.selection_id) || reserved(&delta.selection.source_context))
            && !current.selection_history.contains(delta)
        {
            return Err(archive_conflict(
                &delta.selection.selection_id,
                "existing reserved selection history",
            ));
        }
    }
    validate_receipts(current, incoming)
}

fn validate_receipts(
    current: &SemanticRevisionState,
    incoming: &SemanticRevisionState,
) -> SemanticRevisionResult<()> {
    for operation in &incoming.accepted_operations {
        let receipt = &operation.receipt;
        let protected = reserved(&receipt.operation_id)
            || reserved(&receipt.selection.selection_id)
            || reserved(&receipt.selection.source_context)
            || receipt
                .producer_base
                .as_ref()
                .is_some_and(|base| reserved(&base.selection_id));
        if protected && !current.accepted_operations.contains(operation) {
            return Err(archive_conflict(
                &receipt.operation_id,
                "an identical existing source-owned acceptance receipt",
            ));
        }
    }
    Ok(())
}

fn reserved(id: &str) -> bool {
    id.starts_with("knowledge-citation:") || id.starts_with("challenge-contribution:")
}
