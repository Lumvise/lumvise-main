//! A checkout's pinned editing base advances with the accepted scope, never the entire shared head.
use crate::publication::{publication_conflict, replace_publication_membership};
use crate::publication_records::PreparedRevisionRecords;
use crate::*;

pub(crate) fn validate_producer_base(
    request: &SemanticRevisionPublication,
) -> SemanticRevisionResult<()> {
    let Some(base) = &request.producer_base else {
        return Ok(());
    };
    crate::citation_retention::require_general_selection(&base.selection_id)?;
    if base.selection_id.trim().is_empty()
        || base.selection_id == request.selection_id
        || base.expected_generation < 0
    {
        return Err(SemanticRevisionError::invalid_value(
            format!("{base:?}"),
            "a nonempty producer pin distinct from the shared head and a nonnegative generation",
        ));
    }
    Ok(())
}

pub(crate) fn producer_base_conflict(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
) -> SemanticRevisionResult<Option<SemanticRevisionConflict>> {
    let Some(base) = &request.producer_base else {
        return Ok(None);
    };
    let Some(head) = state.selection(&base.selection_id) else {
        return Ok(
            (base.expected_generation != 0).then(|| generation_conflict(state, request, base, 0))
        );
    };
    if head.selection.mode != SemanticSelectionMode::Pinned
        || head.selection.source_context != request.source_context
    {
        return Err(SemanticRevisionError::invalid_value(
            &base.selection_id,
            "a producer pin in this publication's source context",
        ));
    }
    if head.selection.generation != base.expected_generation {
        return Ok(Some(generation_conflict(
            state,
            request,
            base,
            head.selection.generation,
        )));
    }
    publication_conflict(state, request, head, base.expected_generation)
}

pub(crate) fn advance_producer_base(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    structure: &PreparedRevisionRecords,
    artifacts: &PreparedRevisionRecords,
) -> SemanticRevisionResult<Option<SemanticRevisionSelectionState>> {
    let Some(base) = &request.producer_base else {
        return Ok(None);
    };
    let mut head = state
        .selection(&base.selection_id)
        .cloned()
        .unwrap_or_else(|| empty_producer_base(request, base));
    // A composite checkout does not acquire a clean Git anchor from another writer's head.
    replace_publication_membership(&mut head, structure, artifacts, None)?;
    Ok(Some(head))
}

fn empty_producer_base(
    request: &SemanticRevisionPublication,
    base: &SemanticProducerBase,
) -> SemanticRevisionSelectionState {
    SemanticRevisionSelectionState {
        selection: SemanticRevisionSelection {
            project_root: request.project_root.clone(),
            selection_id: base.selection_id.clone(),
            source_context: request.source_context.clone(),
            mode: SemanticSelectionMode::Pinned,
            generation: 0,
            source_anchor: None,
        },
        records: vec![],
        connection_ids: vec![],
    }
}

fn generation_conflict(
    state: &SemanticRevisionState,
    request: &SemanticRevisionPublication,
    base: &SemanticProducerBase,
    actual_generation: i64,
) -> SemanticRevisionConflict {
    SemanticRevisionConflict {
        project_root: request.project_root.clone(),
        expected_project_id: request.project_id.clone(),
        actual_project_id: state.project_id.clone(),
        selection_id: base.selection_id.clone(),
        expected_generation: base.expected_generation,
        actual_generation,
        records: vec![],
        connection_ids: vec![],
    }
}
