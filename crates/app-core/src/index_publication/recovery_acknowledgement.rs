//! Replays the original repair publication and pin selection before accepting the local checkpoint.
use super::recovery_observation::RecoveredSourceObservation;
use super::recovery_publication::{RecoveredSourcePinConflict, RecoveredSourcePublication};
use super::*;
use lumvise_db_core::{
    SemanticRecordKind, SemanticRecordRef, SemanticRevisionConnectionQuery,
    SemanticRevisionPublicationOutcome, SemanticRevisionPublicationReceipt,
    SemanticRevisionSelectionQuery, SemanticRevisionSelectionUpdate,
    SemanticRevisionStructureCoverage,
};
use std::collections::{BTreeMap, BTreeSet};

struct RecoveredPinSelection {
    selection_id: String,
    generation: i64,
    records: Vec<SemanticRecordRef>,
    connection_ids: Vec<String>,
    artifact_edits: BTreeMap<String, u64>,
}

impl IndexPublicationJournal {
    pub(super) fn acknowledge_recovered(
        &self,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        let Some((header, publication)) = self.acknowledged_pending(receipt, control)? else {
            return Ok(());
        };
        let mut recovery = self.require_recovery_source(&publication, receipt, control)?;
        let changes = self.prepare_acknowledged_scope(&publication, receipt, control)?;
        self.prepare_recovery_pin(&mut recovery, receipt, control)?;
        self.accept_recovery_pin(&mut recovery, control)?;
        let producer_generation =
            verified_pin_generation(&recovery, &self.rows.producer_selection_id())?;
        self.acknowledge_ready(&header, changes, receipt, producer_generation, control)
    }

    fn require_recovery_source(
        &self,
        publication: &FrozenIndexPublication,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<RecoveredSourcePublication> {
        let recovery = self
            .recovered_publication(&publication.operation_id, control)?
            .ok_or_else(|| {
                invalid_checkpoint(
                    &publication.operation_id,
                    "a durable source recovery intent",
                )
            })?;
        recovery.require_frozen(publication)?;
        self.validate_recovery_source_receipt(&recovery, publication, receipt, control)?;
        Ok(recovery)
    }

    fn validate_recovery_source_receipt(
        &self,
        recovery: &RecoveredSourcePublication,
        publication: &FrozenIndexPublication,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        require_original_source_receipt(recovery, receipt)?;
        super::acknowledgement::validate_receipt(
            &self.binding,
            publication,
            receipt,
            self.header(control)?.generation,
        )?;
        Ok(())
    }

    fn prepare_recovery_pin(
        &self,
        recovery: &mut RecoveredSourcePublication,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        if recovery.pin_update.is_some() {
            return self.drop_undefined_pin_connections(recovery, receipt, control);
        }
        let mut next = recovery.clone();
        next.source_receipt = Some(receipt.clone());
        next.pin_update =
            Some(self.recovery_pin_update(recovery, &recovery.observed, receipt, control)?);
        self.save_recovery_phase(recovery, &next, control)?;
        *recovery = next;
        Ok(())
    }

    /// Semantic refuses a pin naming an undefined connection before writing anything, so a pin
    /// frozen before lost definitions were filtered can drop those IDs and run once again.
    fn drop_undefined_pin_connections(
        &self,
        recovery: &mut RecoveredSourcePublication,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        if recovery.pin_receipt.is_some() {
            return Ok(());
        }
        let Some(update) = recovery.pin_update.as_ref() else {
            return Ok(());
        };
        let accepted: BTreeSet<&String> = receipt.accepted_scope.connection_ids.iter().collect();
        let defined = self.defined_connections(
            update
                .connection_ids
                .iter()
                .filter(|id| !accepted.contains(id)),
            control,
        )?;
        let keep = |id: &String| accepted.contains(id) || defined.contains(id);
        if update.connection_ids.iter().all(keep) {
            return Ok(());
        }
        let mut next = recovery.clone();
        if let Some(update) = next.pin_update.as_mut() {
            update.connection_ids.retain(keep);
        }
        self.save_recovery_phase(recovery, &next, control)?;
        *recovery = next;
        Ok(())
    }

    fn recovery_pin_update(
        &self,
        recovery: &RecoveredSourcePublication,
        observed: &RecoveredSourceObservation,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<SemanticRevisionSelectionUpdate> {
        let selection = self.recovery_pin_selection(observed, receipt, control)?;
        Ok(build_recovery_pin_update(
            self, recovery, selection, receipt,
        ))
    }

    fn recovery_pin_selection(
        &self,
        observed: &RecoveredSourceObservation,
        receipt: &SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<RecoveredPinSelection> {
        let generation = observed_producer_generation(observed);
        let previous = self.recovery_producer_members(observed, control)?;
        let (records, mut connection_ids) = recovered_pin_members(&previous, observed, receipt);
        // A lost publication can leave producer history naming connections without a stored
        // definition; the repair pin carries only defined ones plus what the source just accepted.
        let accepted: BTreeSet<&String> = receipt.accepted_scope.connection_ids.iter().collect();
        let defined = self.defined_connections(
            connection_ids.iter().filter(|id| !accepted.contains(id)),
            control,
        )?;
        connection_ids.retain(|id| accepted.contains(id) || defined.contains(id));
        let artifact_edits = self.producer_artifact_edits(&records, generation, control)?;
        Ok(RecoveredPinSelection {
            selection_id: previous.selection.selection_id,
            generation,
            records,
            connection_ids,
            artifact_edits,
        })
    }

    /// Looks up only the carried-over candidates by id; a whole-project membership read decodes
    /// the project's entire revision history and outlives the acknowledgement's deadline.
    fn defined_connections<'a>(
        &self,
        candidates: impl Iterator<Item = &'a String>,
        control: &InvocationControl,
    ) -> Result<BTreeSet<String>> {
        let candidates: Vec<String> = candidates.cloned().collect();
        if candidates.is_empty() {
            return Ok(BTreeSet::new());
        }
        if let Some(edges) = self.retained_connections(candidates.clone(), control)? {
            return Ok(edges);
        }
        let mut defined = BTreeSet::new();
        for id in candidates {
            defined.extend(
                self.retained_connections(vec![id], control)?
                    .unwrap_or_default(),
            );
        }
        Ok(defined)
    }

    /// `None` when any requested connection has no retained definition; other failures propagate.
    fn retained_connections(
        &self,
        connection_ids: Vec<String>,
        control: &InvocationControl,
    ) -> Result<Option<BTreeSet<String>>> {
        let query = SemanticRevisionConnectionQuery {
            project_root: self.project_root.clone(),
            project_id: self.binding.project_id.clone(),
            connection_ids,
        };
        let control = self.invocations.operation(control);
        match self
            .semantic
            .execute(SemanticOperation::RevisionConnections(query), &control)
        {
            Ok(SemanticResult::RevisionConnections(edges)) => Ok(Some(
                edges.into_iter().map(|edge| edge.connection_id).collect(),
            )),
            Err(lumvise_db_core::DbError::InvalidValue { expected, .. })
                if expected == "an exact retained connection definition" =>
            {
                Ok(None)
            }
            Ok(_) => Err(invalid_checkpoint(
                &self.binding.checkout_id,
                "exact retained connection definitions",
            )),
            Err(error) => Err(error.into()),
        }
    }

    fn recovery_producer_members(
        &self,
        observed: &RecoveredSourceObservation,
        control: &InvocationControl,
    ) -> Result<lumvise_db_core::SemanticRevisionSelectionState> {
        if let Some(producer) = &observed.producer {
            return self.producer_members_at(producer.selection.generation, control);
        }
        Ok(self.empty_producer_members())
    }

    fn empty_producer_members(&self) -> lumvise_db_core::SemanticRevisionSelectionState {
        lumvise_db_core::SemanticRevisionSelectionState {
            selection: lumvise_db_core::SemanticRevisionSelection {
                project_root: self.project_root.clone(),
                selection_id: self.rows.producer_selection_id(),
                source_context: self.binding.source_context.clone(),
                mode: lumvise_db_core::SemanticSelectionMode::Pinned,
                generation: 0,
                source_anchor: None,
            },
            records: vec![],
            connection_ids: vec![],
        }
    }

    fn producer_artifact_edits(
        &self,
        records: &[SemanticRecordRef],
        generation: i64,
        control: &InvocationControl,
    ) -> Result<BTreeMap<String, u64>> {
        let artifact_ids = records
            .iter()
            .filter(|record| record.kind == SemanticRecordKind::Artifact)
            .map(|record| record.record_id.clone())
            .collect::<Vec<_>>();
        if artifact_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        self.read_producer_artifact_edits(&artifact_ids, generation, control)
    }

    fn read_producer_artifact_edits(
        &self,
        artifact_ids: &[String],
        generation: i64,
        control: &InvocationControl,
    ) -> Result<BTreeMap<String, u64>> {
        let result = self.producer_artifact_observation(artifact_ids, control)?;
        observed_producer_artifact_edits(result, generation)
    }

    fn producer_artifact_observation(
        &self,
        artifact_ids: &[String],
        control: &InvocationControl,
    ) -> Result<SemanticResult> {
        self.semantic(
            SemanticOperation::RevisionSelectionMembers(SemanticRevisionSelectionQuery {
                project_root: self.project_root.clone(),
                selection_id: self.rows.producer_selection_id(),
                structure_coverage: SemanticRevisionStructureCoverage::None,
                artifact_ids: artifact_ids.to_vec(),
            }),
            control,
        )
    }

    fn accept_recovery_pin(
        &self,
        recovery: &mut RecoveredSourcePublication,
        control: &InvocationControl,
    ) -> Result<()> {
        while recovery.pin_receipt.is_none() {
            require_live_recovery(control, &recovery.operation_id)?;
            match self.execute_recovery_pin(recovery, control)? {
                SemanticRevisionPublicationOutcome::Accepted(receipt) => {
                    self.save_recovery_pin_receipt(recovery, receipt, control)?
                }
                SemanticRevisionPublicationOutcome::Conflict(conflict) => {
                    self.refresh_recovery_pin(recovery, conflict, control)?
                }
            }
        }
        Ok(())
    }

    fn execute_recovery_pin(
        &self,
        recovery: &RecoveredSourcePublication,
        control: &InvocationControl,
    ) -> Result<SemanticRevisionPublicationOutcome> {
        let update = recovery.pin_update.clone().ok_or_else(|| {
            invalid_checkpoint(
                &recovery.operation_id,
                "a frozen producer selection before execution",
            )
        })?;
        let result = self.semantic(SemanticOperation::SelectRevisions(update), control)?;
        let SemanticResult::RevisionPublication(outcome) = result else {
            return Err(invalid_checkpoint(
                &recovery.operation_id,
                "a producer selection publication outcome",
            ));
        };
        Ok(outcome)
    }

    fn save_recovery_pin_receipt(
        &self,
        recovery: &mut RecoveredSourcePublication,
        receipt: SemanticRevisionPublicationReceipt,
        control: &InvocationControl,
    ) -> Result<()> {
        let mut next = recovery.clone();
        next.pin_receipt = Some(receipt);
        verified_pin_generation(&next, &self.rows.producer_selection_id())?;
        self.save_recovery_phase(recovery, &next, control)?;
        *recovery = next;
        Ok(())
    }

    fn refresh_recovery_pin(
        &self,
        recovery: &mut RecoveredSourcePublication,
        conflict: lumvise_db_core::SemanticRevisionConflict,
        control: &InvocationControl,
    ) -> Result<()> {
        let (_, observed) = self.observe_pending_recovery(recovery, control)?;
        let next = self.retry_recovered_pin(recovery, &observed, conflict, control)?;
        self.save_recovery_phase(recovery, &next, control)?;
        *recovery = next;
        Ok(())
    }

    fn observe_pending_recovery(
        &self,
        recovery: &RecoveredSourcePublication,
        control: &InvocationControl,
    ) -> Result<(FrozenIndexPublication, RecoveredSourceObservation)> {
        let publication = self.pending(control)?.ok_or_else(|| {
            invalid_checkpoint(&recovery.operation_id, "the original pending source repair")
        })?;
        recovery.require_frozen(&publication)?;
        let observed = self.observe_recovered_scope(&publication.partition(), control)?;
        Ok((publication, observed))
    }

    fn retry_recovered_pin(
        &self,
        recovery: &RecoveredSourcePublication,
        observed: &RecoveredSourceObservation,
        conflict: lumvise_db_core::SemanticRevisionConflict,
        control: &InvocationControl,
    ) -> Result<RecoveredSourcePublication> {
        let source = recovery.source_receipt.as_ref().ok_or_else(|| {
            invalid_checkpoint(&recovery.operation_id, "the accepted source repair receipt")
        })?;
        let mut next = recovery.clone();
        let request = next.pin_update.take().ok_or_else(|| {
            invalid_checkpoint(&recovery.operation_id, "the frozen conflicted pin request")
        })?;
        next.pin_conflicts
            .push(RecoveredSourcePinConflict { request, conflict });
        next.pin_update = Some(self.recovery_pin_update(&next, observed, source, control)?);
        Ok(next)
    }
}

fn require_original_source_receipt(
    recovery: &RecoveredSourcePublication,
    receipt: &SemanticRevisionPublicationReceipt,
) -> Result<()> {
    if receipt.producer_base.is_some()
        || recovery
            .source_receipt
            .as_ref()
            .is_some_and(|saved| saved != receipt)
    {
        return Err(invalid_checkpoint(
            &receipt.operation_id,
            "the original source repair receipt",
        ));
    }
    Ok(())
}

fn observed_producer_artifact_edits(
    result: SemanticResult,
    generation: i64,
) -> Result<BTreeMap<String, u64>> {
    let SemanticResult::RevisionObservation(Some(observed)) = result else {
        return Err(invalid_checkpoint(
            generation.to_string(),
            "the original producer artifact observation",
        ));
    };
    if observed.selection.generation != generation {
        return Err(invalid_checkpoint(
            observed.selection.generation.to_string(),
            format!("producer generation {generation}"),
        ));
    }
    Ok(observed.expected.artifact_edits)
}

fn replace_members<T: Clone + Ord>(
    previous: &[T],
    removed: Option<&[T]>,
    accepted: &[T],
) -> Vec<T> {
    let removed = removed.unwrap_or_default().iter().collect::<BTreeSet<_>>();
    previous
        .iter()
        .filter(|member| !removed.contains(member))
        .chain(accepted)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn recovered_pin_members(
    previous: &lumvise_db_core::SemanticRevisionSelectionState,
    observed: &RecoveredSourceObservation,
    receipt: &SemanticRevisionPublicationReceipt,
) -> (Vec<SemanticRecordRef>, Vec<String>) {
    let removed = observed.producer.as_ref();
    let records = replace_members(
        &previous.records,
        removed.map(|producer| producer.expected.records.as_slice()),
        &receipt.accepted_scope.records,
    );
    let connections = replace_members(
        &previous.connection_ids,
        removed.map(|producer| producer.expected.connection_ids.as_slice()),
        &receipt.accepted_scope.connection_ids,
    );
    (records, connections)
}

fn build_recovery_pin_update(
    journal: &IndexPublicationJournal,
    recovery: &RecoveredSourcePublication,
    selection: RecoveredPinSelection,
    receipt: &SemanticRevisionPublicationReceipt,
) -> SemanticRevisionSelectionUpdate {
    SemanticRevisionSelectionUpdate {
        operation_id: recovery_pin_operation_id(recovery),
        project_root: journal.project_root.clone(),
        project_id: journal.binding.project_id.clone(),
        selection_id: selection.selection_id,
        source_context: journal.binding.source_context.clone(),
        expected_generation: selection.generation,
        artifact_edits: selection.artifact_edits,
        mode: lumvise_db_core::SemanticSelectionMode::Pinned,
        records: selection.records,
        connection_ids: selection.connection_ids,
        source_anchor: receipt.selection.source_anchor.clone(),
    }
}

fn recovery_pin_operation_id(recovery: &RecoveredSourcePublication) -> String {
    format!(
        "source-recovery-pin:{}:{}",
        recovery.operation_id,
        recovery.pin_conflicts.len()
    )
}

fn observed_producer_generation(observed: &RecoveredSourceObservation) -> i64 {
    let Some(producer) = observed.producer.as_ref() else {
        return 0;
    };
    producer.selection.generation
}

fn require_live_recovery(control: &InvocationControl, operation: &str) -> Result<()> {
    if control.is_cancelled() || control.is_expired() {
        return Err(invalid_checkpoint(
            operation,
            "an active invocation before retrying producer reconciliation",
        ));
    }
    Ok(())
}

fn verified_pin_generation(recovery: &RecoveredSourcePublication, pin: &str) -> Result<i64> {
    let update = recovery
        .pin_update
        .as_ref()
        .ok_or_else(|| invalid_checkpoint(pin, "the frozen pin request"))?;
    let receipt = recovery
        .pin_receipt
        .as_ref()
        .ok_or_else(|| invalid_checkpoint(pin, "the actual pin receipt"))?;
    if !pin_receipt_matches_update(recovery, update, receipt, pin) {
        return Err(invalid_checkpoint(
            &receipt.operation_id,
            "the exact accepted producer reconciliation selection",
        ));
    }
    Ok(receipt.selection.generation)
}

fn pin_receipt_matches_update(
    recovery: &RecoveredSourcePublication,
    update: &SemanticRevisionSelectionUpdate,
    receipt: &SemanticRevisionPublicationReceipt,
    pin: &str,
) -> bool {
    pin_receipt_has_exact_selection(update, receipt, pin)
        && update.expected_generation.checked_add(1) == Some(receipt.selection.generation)
        && receipt.producer_base.is_none()
        && receipt.accepted_scope.generation == receipt.selection.generation
        && recovery
            .source_receipt
            .as_ref()
            .is_some_and(|source| receipt.commit_version > source.commit_version)
        && pin_receipt_members_match(update, receipt)
}

fn pin_receipt_has_exact_selection(
    update: &SemanticRevisionSelectionUpdate,
    receipt: &SemanticRevisionPublicationReceipt,
    pin: &str,
) -> bool {
    receipt.operation_id == update.operation_id
        && receipt.selection.selection_id == pin
        && receipt.selection.project_root == update.project_root
        && receipt.selection.source_context == update.source_context
        && receipt.selection.mode == update.mode
}

fn pin_receipt_members_match(
    update: &SemanticRevisionSelectionUpdate,
    receipt: &SemanticRevisionPublicationReceipt,
) -> bool {
    receipt
        .accepted_scope
        .records
        .iter()
        .collect::<BTreeSet<_>>()
        == update.records.iter().collect()
        && receipt
            .accepted_scope
            .connection_ids
            .iter()
            .collect::<BTreeSet<_>>()
            == update.connection_ids.iter().collect()
}
