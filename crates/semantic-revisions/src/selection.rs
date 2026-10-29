//! Selection reads, explicit pins and adapter-side application of accepted deltas.
use crate::revision_index::*;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionState {
    /// Projects contextual attachments for a selection without changing canonical artifact owners.
    /// Example: `state.selected_associations("working")` for a current-view attachment index.
    pub fn selected_associations(&self, selection_id: &str) -> Vec<SemanticRevisionAssociation> {
        let Some(head) = self.selection(selection_id) else {
            return vec![];
        };
        let selected = head.records.iter().collect::<BTreeSet<_>>();
        self.associations
            .iter()
            .filter(|association| {
                association.source_context == head.selection.source_context
                    && selected.contains(&SemanticRecordRef::artifact(&association.artifact_id))
                    && selected.contains(&SemanticRecordRef::element(
                        &association.semantic_element_id,
                    ))
            })
            .cloned()
            .collect()
    }
    /// Finds a current selection. Example: `state.selection("working")`.
    pub fn selection(&self, selection_id: &str) -> Option<&SemanticRevisionSelectionState> {
        self.selections
            .iter()
            .find(|head| head.selection.selection_id == selection_id)
    }

    /// Resolves an exact record, preserving tombstones and scoped ancestry.
    /// Example: `state.resolve(&SemanticRecordRef::element("e17"))`.
    pub fn resolve(&self, record: &SemanticRecordRef) -> Option<SemanticRevisionResolution> {
        let stored = self
            .records
            .iter()
            .find(|stored| &stored.record == record)?;
        Some(SemanticRevisionResolution {
            element_scope: stored.element_scope.clone(),
            record: record.clone(),
            body: stored.body.clone(),
            artifact_state: stored.artifact_state.clone(),
            created_at_ms: stored.created_at_ms,
            pruned_at_ms: stored.pruned_at_ms,
            successors: self
                .revision_links
                .iter()
                .filter(|link| &link.older == record)
                .cloned()
                .collect(),
        })
    }

    /// Plans an explicit selection or pin under a generation check.
    /// Example: `state.plan_selection(&pin, &timestamp, next_commit)?`.
    pub fn plan_selection(
        &self,
        update: &SemanticRevisionSelectionUpdate,
        timestamp: &SemanticRevisionTimestamp,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
        crate::citation_retention::require_general_selection(&update.selection_id)?;
        crate::citation_retention::require_general_selection(&update.operation_id)?;
        crate::citation_retention::require_general_selection(&update.source_context)?;
        self.plan_owned_selection(update, timestamp, commit_version)
    }

    pub(crate) fn plan_owned_selection(
        &self,
        update: &SemanticRevisionSelectionUpdate,
        timestamp: &SemanticRevisionTimestamp,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
        validate_selection_update(self, update)?;
        let mut canonical = update.clone();
        if let Some(operation) = self
            .accepted_operations
            .iter()
            .find(|operation| operation.receipt.operation_id == update.operation_id)
        {
            canonical.project_root = operation.request_root.clone();
        }
        let request_digest = digest(&("selection", canonical))?;
        if let Some(replay) = self.replay(&update.operation_id, &request_digest)? {
            return Ok(replay);
        }
        if let Some(conflict) = project_identity_conflict(
            self,
            &update.project_id,
            &update.selection_id,
            update.expected_generation,
        ) {
            return Ok(empty_plan(
                self,
                SemanticRevisionPublicationOutcome::Conflict(conflict),
            ));
        }
        require_context_selection(
            self,
            &update.selection_id,
            &update.source_context,
            update.mode,
        )?;
        validate_selected_records(self, &update.records, &update.connection_ids)?;
        let changed_edits = crate::artifact_working::conflicting_references(
            self,
            &update.records.iter().cloned().collect(),
            &update.artifact_edits,
        );
        let actual = self
            .selection(&update.selection_id)
            .map_or(0, |head| head.selection.generation);
        if actual != update.expected_generation || !changed_edits.is_empty() {
            return Ok(empty_plan(
                self,
                SemanticRevisionPublicationOutcome::Conflict(SemanticRevisionConflict {
                    project_root: self.project_root.clone(),
                    expected_project_id: update.project_id.clone(),
                    actual_project_id: self.project_id.clone(),
                    selection_id: update.selection_id.clone(),
                    expected_generation: update.expected_generation,
                    actual_generation: actual,
                    records: changed_edits.into_iter().collect(),
                    connection_ids: update.connection_ids.clone(),
                }),
            ));
        }
        let head = SemanticRevisionSelectionState {
            selection: SemanticRevisionSelection {
                project_root: self.project_root.clone(),
                selection_id: update.selection_id.clone(),
                source_context: update.source_context.clone(),
                mode: update.mode,
                generation: actual + 1,
                source_anchor: update.source_anchor.clone(),
            },
            records: update.records.clone(),
            connection_ids: update.connection_ids.clone(),
        };
        accepted_selection_plan(
            self,
            head,
            &update.operation_id,
            request_digest,
            timestamp,
            commit_version,
        )
    }

    /// Applies a planner delta only to the state it examined; adapters must fence it atomically.
    /// Example: `state.apply_publication(plan)?` after acquiring the adapter write transaction.
    pub fn apply_publication(
        &mut self,
        plan: SemanticRevisionPublicationPlan,
    ) -> SemanticRevisionResult<SemanticRevisionPublicationOutcome> {
        if self.commit_version != plan.expected_commit_version {
            return Err(SemanticRevisionError::invalid_value(
                self.commit_version.to_string(),
                format!(
                    "commit {} before applying publication",
                    plan.expected_commit_version
                ),
            ));
        }
        if let Some(operation) = plan.accepted_operation {
            let selections = plan
                .selection_deltas
                .iter()
                .map(|delta| delta.apply_to(self.selection(&delta.selection.selection_id)))
                .collect::<SemanticRevisionResult<Vec<_>>>()?;
            self.commit_version = operation.receipt.commit_version;
            replace_rows(&mut self.records, plan.records, |row| row.record.clone());
            replace_rows(&mut self.connections, plan.connections, |row| {
                row.connection_id.clone()
            });
            self.selection_history.extend(plan.selection_deltas);
            replace_rows(&mut self.selections, selections, |row| {
                row.selection.selection_id.clone()
            });
            replace_rows(&mut self.associations, plan.associations, |row| {
                row.association_id.clone()
            });
            self.revision_links.extend(plan.revision_links);
            self.accepted_operations.push(operation);
        }
        Ok(plan.outcome)
    }

    pub(crate) fn replay(
        &self,
        operation_id: &str,
        request_digest: &str,
    ) -> SemanticRevisionResult<Option<SemanticRevisionPublicationPlan>> {
        if self
            .retention_operations
            .iter()
            .any(|accepted| accepted.receipt.operation_id == operation_id)
        {
            return Err(SemanticRevisionError::invalid_value(
                operation_id,
                "a publication operation ID unused by collection",
            ));
        }
        let Some(previous) = self
            .accepted_operations
            .iter()
            .find(|stored| stored.receipt.operation_id == operation_id)
        else {
            return Ok(None);
        };
        if previous.request_digest != request_digest {
            return Err(SemanticRevisionError::invalid_value(
                operation_id,
                "identical content for an accepted operation ID",
            ));
        }
        Ok(Some(empty_plan(
            self,
            SemanticRevisionPublicationOutcome::Accepted(previous.receipt.clone()),
        )))
    }
}

pub(crate) fn require_context_selection(
    state: &SemanticRevisionState,
    selection_id: &str,
    context: &str,
    mode: SemanticSelectionMode,
) -> SemanticRevisionResult<()> {
    if mode == SemanticSelectionMode::Pinned {
        return Ok(());
    }
    if let Some(owner) = state.selections.iter().find(|head| {
        head.selection.source_context == context
            && head.selection.mode == SemanticSelectionMode::FollowAccepted
            && head.selection.selection_id != selection_id
    }) {
        return Err(SemanticRevisionError::invalid_value(
            selection_id,
            format!(
                "the shared follow-accepted selection `{}` for context `{context}`, a pin, or a distinct source context",
                owner.selection.selection_id
            ),
        ));
    }
    Ok(())
}

pub(crate) fn replace_rows<T, K: Ord>(rows: &mut Vec<T>, incoming: Vec<T>, key: impl Fn(&T) -> K) {
    let mut indexed = rows
        .drain(..)
        .map(|row| (key(&row), row))
        .collect::<BTreeMap<_, _>>();
    for row in incoming {
        indexed.insert(key(&row), row);
    }
    rows.extend(indexed.into_values());
}

fn validate_selection_update(
    state: &SemanticRevisionState,
    update: &SemanticRevisionSelectionUpdate,
) -> SemanticRevisionResult<()> {
    require_project_identity(&state.project_id)?;
    require_project_identity(&update.project_id)?;
    ensure_root(&update.project_root, &state.project_root)?;
    for id in [
        &update.operation_id,
        &update.selection_id,
        &update.source_context,
    ] {
        if id.trim().is_empty() {
            return Err(SemanticRevisionError::invalid_value(
                id,
                "a non-empty selection operation identity",
            ));
        }
    }
    validate_selection_shape(&update.records, &update.connection_ids)
}

fn validate_selection_shape(
    records: &[SemanticRecordRef],
    connection_ids: &[String],
) -> SemanticRevisionResult<()> {
    let mut seen = BTreeSet::new();
    for record in records {
        if !seen.insert(record) {
            return Err(SemanticRevisionError::invalid_value(
                &record.record_id,
                "one occurrence of each selected record",
            ));
        }
    }
    unique_nonempty(connection_ids, "unique non-empty connection IDs")
}

pub(crate) fn validate_selected_records(
    state: &SemanticRevisionState,
    records: &[SemanticRecordRef],
    connection_ids: &[String],
) -> SemanticRevisionResult<()> {
    validate_selection_shape(records, connection_ids)?;
    let index = record_index(state);
    for record in records {
        require_body(&index, record)?;
    }
    for id in connection_ids {
        let edge = state
            .connections
            .iter()
            .find(|edge| &edge.connection_id == id)
            .ok_or_else(|| {
                SemanticRevisionError::invalid_value(id, "an existing project connection")
            })?;
        require_body(
            &index,
            &SemanticRecordRef::element(&edge.relationship.source_element_id),
        )?;
        require_body(
            &index,
            &SemanticRecordRef::element(&edge.relationship.target_element_id),
        )?;
    }
    Ok(())
}

fn accepted_selection_plan(
    state: &SemanticRevisionState,
    head: SemanticRevisionSelectionState,
    operation_id: &str,
    request_digest: String,
    timestamp: &SemanticRevisionTimestamp,
    commit_version: i64,
) -> SemanticRevisionResult<SemanticRevisionPublicationPlan> {
    let receipt = SemanticRevisionPublicationReceipt {
        operation_id: operation_id.into(),
        producer_base: None,
        selection: head.selection.clone(),
        accepted_scope: head.expectation(&state.records),
        commit_version,
        remaps: vec![],
        revision_links: vec![],
    };
    let mut plan = empty_plan(
        state,
        SemanticRevisionPublicationOutcome::Accepted(receipt.clone()),
    );
    let choices = head.records.clone();
    plan.selections.push(head);
    plan.selection_deltas = plan
        .selections
        .iter()
        .map(|next| {
            SemanticRevisionSelectionDelta::between(
                state.selection(&next.selection.selection_id),
                next,
                commit_version,
            )
        })
        .collect();
    plan.records = retirement_changes(state, &plan.selections, &[], timestamp.unix_ms);
    crate::artifact_working::freeze_retained_states(state, &mut plan, &choices);
    plan.accepted_operation = Some(SemanticRevisionAcceptedOperation {
        request_root: state.project_root.clone(),
        request_digest,
        receipt,
    });
    Ok(plan)
}

pub(crate) fn retirement_changes(
    state: &SemanticRevisionState,
    updated: &[SemanticRevisionSelectionState],
    incoming: &[SemanticStoredRevision],
    now_ms: i64,
) -> Vec<SemanticStoredRevision> {
    let updates = updated
        .iter()
        .map(|head| (head.selection.selection_id.as_str(), head))
        .collect::<BTreeMap<_, _>>();
    let selected = state
        .selections
        .iter()
        .filter(|head| !updates.contains_key(head.selection.selection_id.as_str()))
        .chain(updated.iter())
        .flat_map(|head| head.records.iter())
        .collect::<BTreeSet<_>>();
    let mut records = incoming.to_vec();
    let affected = updated
        .iter()
        .flat_map(|head| {
            let previous = state
                .selection(&head.selection.selection_id)
                .into_iter()
                .flat_map(|old| old.records.iter())
                .collect::<BTreeSet<_>>();
            let next = head.records.iter().collect::<BTreeSet<_>>();
            previous
                .symmetric_difference(&next)
                .map(|record| (*record).clone())
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    for old in state
        .records
        .iter()
        .filter(|stored| affected.contains(&stored.record))
    {
        let unselected = if selected.contains(&old.record) {
            None
        } else {
            old.unselected_at_ms.or(Some(now_ms))
        };
        if unselected != old.unselected_at_ms {
            let mut changed = old.clone();
            changed.unselected_at_ms = unselected;
            records.push(changed);
        }
    }
    records
}
