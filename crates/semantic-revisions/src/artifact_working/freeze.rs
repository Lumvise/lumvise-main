//! Freezing retains exact artifact states at source, reference and selection boundaries.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn freeze_retained_states(
    state: &SemanticRevisionState,
    plan: &mut SemanticRevisionPublicationPlan,
    choices: &[SemanticRecordRef],
) {
    let roots = freeze_roots(state, plan, choices);
    let records = state
        .records
        .iter()
        .chain(&plan.records)
        .map(|row| (&row.record, row))
        .collect::<BTreeMap<_, _>>();
    let frozen = records
        .values()
        .filter(|row| needs_freeze(row, &roots))
        .map(|row| frozen_record(row))
        .collect::<Vec<_>>();
    crate::selection::replace_rows(&mut plan.records, frozen, |row| row.record.clone());
}

fn pinned_records(
    state: &SemanticRevisionState,
    updated: &[SemanticRevisionSelectionState],
) -> BTreeSet<SemanticRecordRef> {
    let replaced = updated
        .iter()
        .map(|head| &head.selection.selection_id)
        .collect::<BTreeSet<_>>();
    state
        .selections
        .iter()
        .filter(|head| !replaced.contains(&head.selection.selection_id))
        .chain(updated)
        .filter(|head| head.selection.mode == SemanticSelectionMode::Pinned)
        .flat_map(|head| head.records.iter().cloned())
        .collect()
}

pub(crate) fn freeze_initial_dependencies(state: &mut SemanticRevisionState) {
    let required = state
        .records
        .iter()
        .flat_map(|row| row.body.intrinsic_dependencies())
        .collect::<BTreeSet<_>>();
    for row in &mut state.records {
        if required.contains(&row.record) {
            if let Some(status) = &mut row.artifact_state {
                status.frozen = true;
            }
        }
    }
}

fn advanced_artifacts(
    state: &SemanticRevisionState,
    plan: &SemanticRevisionPublicationPlan,
) -> BTreeSet<SemanticRecordRef> {
    let advanced = plan
        .revision_links
        .iter()
        .filter(|link| link.older.kind == SemanticRecordKind::Element)
        .map(|link| &link.older.record_id)
        .chain(
            plan.records
                .iter()
                .filter(|row| {
                    row.record.kind == SemanticRecordKind::Element && row.unselected_at_ms.is_some()
                })
                .map(|row| &row.record.record_id),
        )
        .collect::<BTreeSet<_>>();
    let mut artifacts = state
        .associations
        .iter()
        .filter(|row| advanced.contains(&row.semantic_element_id))
        .map(|row| SemanticRecordRef::artifact(&row.artifact_id))
        .collect::<BTreeSet<_>>();
    artifacts.extend(state.records.iter().filter_map(|row| match &row.body {
        SemanticRevisionBody::Artifact(artifact)
            if advanced.contains(&artifact.semantic_element_id) =>
        {
            Some(row.record.clone())
        }
        _ => None,
    }));
    artifacts
}

fn frozen_record(record: &SemanticStoredRevision) -> SemanticStoredRevision {
    let mut frozen = record.clone();
    frozen
        .artifact_state
        .as_mut()
        .expect("only artifact states can freeze")
        .frozen = true;
    frozen
}

fn freeze_roots(
    state: &SemanticRevisionState,
    plan: &SemanticRevisionPublicationPlan,
    choices: &[SemanticRecordRef],
) -> BTreeSet<SemanticRecordRef> {
    let mut roots = state
        .records
        .iter()
        .chain(&plan.records)
        .flat_map(|row| row.body.intrinsic_dependencies())
        .chain(choices.iter().cloned())
        .collect::<BTreeSet<_>>();
    roots.extend(pinned_records(state, &plan.selections));
    roots.extend(advanced_artifacts(state, plan));
    roots
}

fn needs_freeze(row: &SemanticStoredRevision, roots: &BTreeSet<SemanticRecordRef>) -> bool {
    row.artifact_state
        .as_ref()
        .is_some_and(|status| !status.frozen)
        && (row.unselected_at_ms.is_some() || roots.contains(&row.record))
}
