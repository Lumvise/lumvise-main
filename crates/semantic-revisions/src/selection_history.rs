//! Membership history owns exact source-anchor reconstruction, independently of retained bodies.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One accepted membership change; unchanged records and connections have no history payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticRevisionSelectionDelta {
    pub selection: SemanticRevisionSelection,
    pub previous_generation: i64,
    pub commit_version: i64,
    pub added_records: Vec<SemanticRecordRef>,
    pub removed_records: Vec<SemanticRecordRef>,
    pub added_connection_ids: Vec<String>,
    pub removed_connection_ids: Vec<String>,
}

impl SemanticRevisionSelectionDelta {
    /// Captures only changed membership. Example: `SemanticRevisionSelectionDelta::between(None, &head, 1)`.
    pub fn between(
        previous: Option<&SemanticRevisionSelectionState>,
        next: &SemanticRevisionSelectionState,
        commit_version: i64,
    ) -> Self {
        let (old_records, old_connections) = previous.map_or((&[][..], &[][..]), |head| {
            (&head.records, &head.connection_ids)
        });
        Self {
            selection: next.selection.clone(),
            previous_generation: previous.map_or(0, |head| head.selection.generation),
            commit_version,
            added_records: additions(old_records, &next.records),
            removed_records: additions(&next.records, old_records),
            added_connection_ids: additions(old_connections, &next.connection_ids),
            removed_connection_ids: additions(&next.connection_ids, old_connections),
        }
    }

    /// Reconstructs exact membership without hydrating bodies. Example: `delta.apply_to(None)?` for the seed.
    pub fn apply_to(
        &self,
        previous: Option<&SemanticRevisionSelectionState>,
    ) -> SemanticRevisionResult<SemanticRevisionSelectionState> {
        let generation = previous.map_or(0, |head| head.selection.generation);
        if generation != self.previous_generation || self.selection.generation != generation + 1 {
            return Err(SemanticRevisionError::invalid_value(
                format!("{}:{generation}", self.selection.selection_id),
                format!(
                    "contiguous membership history before generation {}",
                    self.selection.generation
                ),
            ));
        }
        let (records, connections) = previous.map_or((&[][..], &[][..]), |head| {
            (&head.records, &head.connection_ids)
        });
        Ok(SemanticRevisionSelectionState {
            selection: self.selection.clone(),
            records: changed_membership(records, &self.added_records, &self.removed_records)?,
            connection_ids: changed_membership(
                connections,
                &self.added_connection_ids,
                &self.removed_connection_ids,
            )?,
        })
    }
}

impl SemanticRevisionState {
    /// Reconstructs an exact historical selection, including IDs whose payloads were pruned.
    /// Example: `state.selection_at_generation("working", 3)?`.
    pub fn selection_at_generation(
        &self,
        selection_id: &str,
        generation: i64,
    ) -> SemanticRevisionResult<Option<SemanticRevisionSelectionState>> {
        let mut history = self
            .selection_history
            .iter()
            .filter(|delta| {
                delta.selection.selection_id == selection_id
                    && delta.selection.generation <= generation
            })
            .collect::<Vec<_>>();
        history.sort_by_key(|delta| delta.selection.generation);
        let mut selected = None;
        for delta in history {
            selected = Some(delta.apply_to(selected.as_ref())?);
        }
        Ok(selected.filter(|head| head.selection.generation == generation))
    }

    /// Finds indexed clean source content instead of minting another revision of an old checkout.
    /// Example: `state.selection_for_source_anchor("working", &anchor)?`.
    pub fn selection_for_source_anchor(
        &self,
        selection_id: &str,
        anchor: &SemanticSourceAnchor,
    ) -> SemanticRevisionResult<Option<SemanticRevisionSelectionState>> {
        if anchor.dirty {
            return Err(SemanticRevisionError::invalid_value(
                &anchor.revision,
                "a clean source anchor",
            ));
        }
        let generation = self
            .selection_history
            .iter()
            .filter(|delta| {
                delta.selection.selection_id == selection_id
                    && delta.selection.source_anchor.as_ref() == Some(anchor)
            })
            .map(|delta| delta.selection.generation)
            .max();
        generation.map_or(Ok(None), |generation| {
            self.selection_at_generation(selection_id, generation)
        })
    }
}

fn additions<T: Ord + Clone>(old: &[T], new: &[T]) -> Vec<T> {
    let old = old.iter().collect::<BTreeSet<_>>();
    new.iter()
        .filter(|member| !old.contains(member))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn changed_membership<T: Ord + Clone + std::fmt::Debug>(
    previous: &[T],
    added: &[T],
    removed: &[T],
) -> SemanticRevisionResult<Vec<T>> {
    let mut members = previous.iter().cloned().collect::<BTreeSet<_>>();
    for member in removed {
        if !members.remove(member) {
            return Err(SemanticRevisionError::invalid_value(
                format!("{member:?}"),
                "a previously selected member to remove",
            ));
        }
    }
    for member in added {
        if !members.insert(member.clone()) {
            return Err(SemanticRevisionError::invalid_value(
                format!("{member:?}"),
                "a newly selected member to add",
            ));
        }
    }
    Ok(members.into_iter().collect())
}
