//! Configured eligibility plus rooted reachability; navigation alone never keeps every old body alive.
use crate::required_revisions::RevisionDependencyIndex;
use crate::retention_eligibility::eligible_history_records;
use crate::revision_index::*;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

impl SemanticRevisionState {
    /// Proposes collection of unneeded bodies, fenced against publication and new pins.
    /// Example: `state.plan_retention("sweep-1", &policy, now_ms)?`.
    pub fn plan_retention(
        &self,
        operation_id: &str,
        policy: &SemanticRevisionRetentionPolicy,
        now_ms: i64,
    ) -> SemanticRevisionResult<SemanticRevisionRetentionPlan> {
        crate::citation_retention::require_general_selection(operation_id)?;
        require_project_identity(&self.project_id)?;
        if operation_id.trim().is_empty() {
            return Err(SemanticRevisionError::invalid_value(
                operation_id,
                "a non-empty sweep operation ID",
            ));
        }
        let eligible = eligible_history_records(self, policy, now_ms)?;
        let expired = expiring_associations(self, &eligible);
        let retained = retained_closure(self, &eligible, &expired)?;
        let records = eligible.difference(&retained).cloned().collect();
        Ok(SemanticRevisionRetentionPlan {
            operation_id: operation_id.into(),
            project_root: self.project_root.clone(),
            project_id: self.project_id.clone(),
            expected_commit_version: self.commit_version,
            evaluated_at_ms: now_ms,
            records,
            expired_association_ids: expired.into_iter().collect(),
        })
    }

    /// Applies only a freshly recomputed, complete sweep; no retained record can acquire dangling dependencies.
    /// Example: `state.apply_retention(&plan, &policy, now_ms, next_commit)?`.
    pub fn apply_retention(
        &mut self,
        plan: &SemanticRevisionRetentionPlan,
        policy: &SemanticRevisionRetentionPolicy,
        now_ms: i64,
        commit_version: i64,
    ) -> SemanticRevisionResult<SemanticRevisionRetentionReceipt> {
        ensure_root(&plan.project_root, &self.project_root)?;
        require_project_identity(&plan.project_id)?;
        let mut canonical = plan.clone();
        if let Some(operation) = self
            .retention_operations
            .iter()
            .find(|operation| operation.receipt.operation_id == plan.operation_id)
        {
            canonical.project_root = operation.request_root.clone();
        }
        let request_digest = digest(&(&canonical, policy))?;
        if let Some(previous) = self
            .retention_operations
            .iter()
            .find(|accepted| accepted.receipt.operation_id == plan.operation_id)
        {
            if previous.request_digest != request_digest {
                return Err(SemanticRevisionError::invalid_value(
                    &plan.operation_id,
                    "identical content for an accepted collection operation ID",
                ));
            }
            return Ok(previous.receipt.clone());
        }
        if self
            .accepted_operations
            .iter()
            .any(|accepted| accepted.receipt.operation_id == plan.operation_id)
        {
            return Err(SemanticRevisionError::invalid_value(
                &plan.operation_id,
                "a collection operation ID unused by publication",
            ));
        }

        if plan.project_id != self.project_id {
            return Err(SemanticRevisionError::invalid_value(
                &plan.project_id,
                format!(
                    "the observed project lineage `{}` before collection",
                    self.project_id
                ),
            ));
        }
        if plan.evaluated_at_ms > now_ms {
            return Err(SemanticRevisionError::invalid_value(
                plan.evaluated_at_ms.to_string(),
                "a collection plan made no later than the current time",
            ));
        }
        let expected = self.plan_retention(&plan.operation_id, policy, plan.evaluated_at_ms)?;
        if &expected != plan || commit_version <= self.commit_version {
            return Err(SemanticRevisionError::invalid_value(
                plan.operation_id.clone(),
                "the unchanged collection plan and commit state; replan after a publication or new pin",
            ));
        }
        let pruned = plan.records.iter().collect::<BTreeSet<_>>();
        let removed_blob_refs = unreferenced_content_refs(self, &pruned);
        for stored in &mut self.records {
            if pruned.contains(&stored.record) {
                stored.body = SemanticRevisionBody::Pruned;
                stored.pruned_at_ms = Some(now_ms);
            }
        }
        let associations = plan.expired_association_ids.iter().collect::<BTreeSet<_>>();
        let before = self.associations.len();
        self.associations.retain(|association| {
            !associations.contains(&association.association_id)
                && !pruned.contains(&SemanticRecordRef::artifact(&association.artifact_id))
        });
        // Connection definitions reconstruct historical membership even after endpoint payloads expire.
        self.commit_version = commit_version;
        let receipt = SemanticRevisionRetentionReceipt {
            operation_id: plan.operation_id.clone(),
            commit_version,
            pruned_records: plan.records.clone(),
            removed_blob_refs,
            removed_associations: before - self.associations.len(),
        };
        self.retention_operations
            .push(SemanticRevisionAcceptedRetention {
                request_root: self.project_root.clone(),
                request_digest,
                receipt: receipt.clone(),
            });
        Ok(receipt)
    }
}

fn expiring_associations(
    state: &SemanticRevisionState,
    eligible: &BTreeSet<SemanticRecordRef>,
) -> BTreeSet<String> {
    let selected = state
        .selections
        .iter()
        .flat_map(|head| head.records.iter())
        .collect::<BTreeSet<_>>();
    let current = state
        .associations
        .iter()
        .filter(|association| {
            selected.contains(&SemanticRecordRef::element(
                &association.semantic_element_id,
            ))
        })
        .map(|association| (&association.source_context, &association.artifact_id))
        .collect::<BTreeSet<_>>();
    state
        .associations
        .iter()
        .filter(|association| {
            let owner = SemanticRecordRef::element(&association.semantic_element_id);
            eligible.contains(&owner)
                && !selected.contains(&owner)
                && current.contains(&(&association.source_context, &association.artifact_id))
        })
        .map(|association| association.association_id.clone())
        .collect()
}

fn retained_closure(
    state: &SemanticRevisionState,
    eligible: &BTreeSet<SemanticRecordRef>,
    expired: &BTreeSet<String>,
) -> SemanticRevisionResult<BTreeSet<SemanticRecordRef>> {
    let mut pending = state
        .selections
        .iter()
        .flat_map(|head| head.records.iter().cloned())
        .collect::<Vec<_>>();
    pending.extend(
        state
            .records
            .iter()
            .filter(|stored| {
                !eligible.contains(&stored.record)
                    && !matches!(stored.body, SemanticRevisionBody::Pruned)
            })
            .map(|stored| stored.record.clone()),
    );
    let connections = state
        .selections
        .iter()
        .flat_map(|head| head.connection_ids.iter())
        .collect::<BTreeSet<_>>();
    for edge in state
        .connections
        .iter()
        .filter(|edge| connections.contains(&edge.connection_id))
    {
        pending.push(SemanticRecordRef::element(
            &edge.relationship.source_element_id,
        ));
        pending.push(SemanticRecordRef::element(
            &edge.relationship.target_element_id,
        ));
    }
    let index = RevisionDependencyIndex::new(state);
    let mut retained = BTreeSet::new();
    while let Some(record) = pending.pop() {
        if !retained.insert(record.clone()) {
            continue;
        }
        let stored = index.require(&record)?;
        pending.extend(index.requirements(stored, expired));
    }
    Ok(retained)
}

fn unreferenced_content_refs(
    state: &SemanticRevisionState,
    pruned: &BTreeSet<&SemanticRecordRef>,
) -> Vec<String> {
    let mut owned = BTreeMap::<String, (bool, bool)>::new();
    for stored in &state.records {
        let SemanticRevisionBody::Artifact(artifact) = &stored.body else {
            continue;
        };
        let Some(content_ref) = &artifact.content_ref else {
            continue;
        };
        let ownership = owned.entry(content_ref.clone()).or_default();
        if pruned.contains(&stored.record) {
            ownership.0 = true;
        } else {
            ownership.1 = true;
        }
    }
    owned
        .into_iter()
        .filter(|(_, (removed, kept))| *removed && !kept)
        .map(|(reference, _)| reference)
        .collect()
}
