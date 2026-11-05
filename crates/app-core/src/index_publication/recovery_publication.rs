//! Owns the frozen two-receipt source repair intent; normal source acknowledgement stays strict.
use super::recovery_observation::RecoveredSourceObservation;
use super::*;
use lumvise_db_core::{SemanticRevisionPublicationReceipt, SemanticRevisionSelectionUpdate};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveredSourcePublication {
    pub(super) operation_id: String,
    pub(super) frozen_digest: String,
    pub(super) observed: RecoveredSourceObservation,
    pub(super) unavailable_headers: Vec<lumvise_db_core::SemanticRevisionRecordHeader>,
    pub(super) source_receipt: Option<SemanticRevisionPublicationReceipt>,
    pub(super) pin_update: Option<SemanticRevisionSelectionUpdate>,
    pub(super) pin_receipt: Option<SemanticRevisionPublicationReceipt>,
    pub(super) pin_conflicts: Vec<RecoveredSourcePinConflict>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveredSourcePinConflict {
    pub(super) request: SemanticRevisionSelectionUpdate,
    pub(super) conflict: lumvise_db_core::SemanticRevisionConflict,
}

impl RecoveredSourcePublication {
    pub(super) fn require_frozen(&self, publication: &FrozenIndexPublication) -> Result<()> {
        if self.operation_id != publication.operation_id
            || self.frozen_digest != pending::publication_digest(publication)?
            || publication.observation()?.producer_base.is_some()
            || self.unavailable_headers.is_empty()
        {
            return Err(invalid_checkpoint(
                &publication.operation_id,
                "the exact frozen source repair and retained unavailable evidence",
            ));
        }
        Ok(())
    }
}

impl IndexPublicationJournal {
    pub(super) fn recovered_publication(
        &self,
        operation: &str,
        control: &InvocationControl,
    ) -> Result<Option<RecoveredSourcePublication>> {
        self.rows.get(&recovery_key(operation), control)
    }

    pub(super) fn prepare_recovered_publication(
        &self,
        publication: &FrozenIndexPublication,
        observed: RecoveredSourceObservation,
        unavailable_headers: Vec<lumvise_db_core::SemanticRevisionRecordHeader>,
    ) -> Result<RecoveredSourcePublication> {
        let recovery = RecoveredSourcePublication {
            operation_id: publication.operation_id.clone(),
            frozen_digest: pending::publication_digest(publication)?,
            observed,
            unavailable_headers,
            source_receipt: None,
            pin_update: None,
            pin_receipt: None,
            pin_conflicts: vec![],
        };
        recovery.require_frozen(publication)?;
        Ok(recovery)
    }

    pub(super) fn recovery_intent_mutations(
        &self,
        recovery: &RecoveredSourcePublication,
        control: &InvocationControl,
    ) -> Result<Vec<lumvise_db_core::PluginDataMutation>> {
        let key = recovery_key(&recovery.operation_id);
        let previous = self.recovered_publication(&recovery.operation_id, control)?;
        if previous.as_ref().is_some_and(|saved| saved != recovery) {
            return Err(invalid_checkpoint(
                &recovery.operation_id,
                "the unchanged source recovery intent",
            ));
        }
        Ok(vec![
            self.rows.require_value(&key, previous.as_ref())?,
            self.rows.put(&key, recovery)?,
        ])
    }

    pub(super) fn save_recovery_phase(
        &self,
        previous: &RecoveredSourcePublication,
        next: &RecoveredSourcePublication,
        control: &InvocationControl,
    ) -> Result<()> {
        let header = self.header(control)?;
        require_recovery_pending(&header, previous)?;
        let key = recovery_key(&previous.operation_id);
        self.rows.apply(
            vec![
                self.rows.require(Some(&header))?,
                self.rows.require_value(&key, Some(previous))?,
                self.rows.put(&key, next)?,
            ],
            control,
        )
    }

    pub(super) fn require_recovered_observation(
        &self,
        publication: &FrozenIndexPublication,
        recovery: &RecoveredSourcePublication,
        control: &InvocationControl,
    ) -> Result<()> {
        recovery.require_frozen(publication)?;
        let current = self.observe_recovered_scope(&publication.partition(), control)?;
        if publication.observation()? != &recovery.observed.publication_observation(&self.binding)
            || !current.matches_frozen(publication.observation()?)
            || current.producer != recovery.observed.producer
        {
            return Err(invalid_checkpoint(
                &publication.operation_id,
                "unchanged working and producer membership before freezing source repair",
            ));
        }
        Ok(())
    }
}

pub(super) fn recovery_key(operation: &str) -> String {
    format!("recovery/{operation}")
}

fn require_recovery_pending(
    header: &CheckpointHeader,
    recovery: &RecoveredSourcePublication,
) -> Result<()> {
    if header.pending.as_ref().is_none_or(|pending| {
        !pending.ready
            || pending.operation_id != recovery.operation_id
            || pending.digest != recovery.frozen_digest
    }) {
        return Err(invalid_checkpoint(
            &recovery.operation_id,
            "the same ready source recovery operation",
        ));
    }
    Ok(())
}
