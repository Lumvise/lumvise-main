//! Reads source recovery membership through portable semantic operations only.
use super::{IndexCheckoutBinding, IndexPublicationJournal, invalid_checkpoint};
use crate::Result;
use lumvise_contracts::SemanticIndexObservation;
use lumvise_db_core::{
    SemanticOperation, SemanticPartition, SemanticResult, SemanticRevisionSelectionObservation,
    SemanticRevisionSelectionQuery, SemanticRevisionSelectionState,
    SemanticRevisionStructureCoverage, SemanticSelectionMode,
};
use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct RecoveredSourceObservation {
    pub(super) working: SemanticRevisionSelectionObservation,
    pub(super) producer: Option<SemanticRevisionSelectionObservation>,
}

impl RecoveredSourceObservation {
    pub(super) fn publication_observation(
        &self,
        binding: &IndexCheckoutBinding,
    ) -> SemanticIndexObservation {
        SemanticIndexObservation {
            project_id: binding.project_id.clone(),
            selection_id: binding.selection_id.clone(),
            source_context: binding.source_context.clone(),
            producer_base: None,
            expected: self.working.expected.clone(),
        }
    }

    pub(super) fn matches_frozen(&self, observation: &SemanticIndexObservation) -> bool {
        if self.working.selection.selection_id != observation.selection_id
            || self.working.selection.source_context != observation.source_context
            || !same_scoped_members(&self.working.expected, &observation.expected)
        {
            return false;
        }
        let Some(base) = &observation.producer_base else {
            return true;
        };
        self.producer.as_ref().is_some_and(|producer| {
            producer.selection.selection_id == base.selection_id
                && producer.selection.source_context == observation.source_context
                && producer.selection.mode == SemanticSelectionMode::Pinned
                && producer.selection.generation == base.expected_generation
                && same_scoped_members(&producer.expected, &observation.expected)
        })
    }
}

impl IndexPublicationJournal {
    pub(super) fn observe_recovered_scope(
        &self,
        partition: &SemanticPartition,
        control: &InvocationControl,
    ) -> Result<RecoveredSourceObservation> {
        self.validate_partition(partition)?;
        self.validate_project_identity(control)?;
        let before = self.read_recovered_scope(partition, control)?;
        let after = self.read_recovered_scope(partition, control)?;
        self.validate_project_identity(control)?;
        if before != after {
            return Err(invalid_checkpoint(
                &self.binding.checkout_id,
                "stable working and producer memberships during recovery inspection",
            ));
        }
        Ok(after)
    }

    pub(super) fn producer_members_at(
        &self,
        generation: i64,
        control: &InvocationControl,
    ) -> Result<SemanticRevisionSelectionState> {
        if generation < 0 {
            return Err(invalid_checkpoint(
                generation.to_string(),
                "a non-negative pin generation",
            ));
        }
        self.validate_project_identity(control)?;
        let state = self.read_producer_members_at(generation, control)?;
        self.validate_project_identity(control)?;
        self.validate_producer_state(&state, generation)?;
        Ok(state)
    }

    fn read_recovered_scope(
        &self,
        partition: &SemanticPartition,
        control: &InvocationControl,
    ) -> Result<RecoveredSourceObservation> {
        let working =
            self.read_selection_members(&self.binding.selection_id, partition, control)?;
        let working = working.ok_or_else(|| {
            invalid_checkpoint(
                &self.binding.selection_id,
                "an established working selection",
            )
        })?;
        self.validate_selection_observation(&working, &self.binding.selection_id, false)?;
        let producer_id = self.rows.producer_selection_id();
        let producer = self.read_selection_members(&producer_id, partition, control)?;
        if let Some(observation) = &producer {
            self.validate_selection_observation(observation, &producer_id, true)?;
        }
        Ok(RecoveredSourceObservation { working, producer })
    }

    fn read_selection_members(
        &self,
        selection_id: &str,
        partition: &SemanticPartition,
        control: &InvocationControl,
    ) -> Result<Option<SemanticRevisionSelectionObservation>> {
        let query = SemanticRevisionSelectionQuery {
            project_root: self.project_root.clone(),
            selection_id: selection_id.into(),
            structure_coverage: SemanticRevisionStructureCoverage::Partition(partition.clone()),
            artifact_ids: vec![],
        };
        match self.semantic(SemanticOperation::RevisionSelectionMembers(query), control)? {
            SemanticResult::RevisionObservation(observation) => Ok(observation),
            _ => Err(invalid_checkpoint(
                selection_id,
                "a membership-only revision observation",
            )),
        }
    }

    fn read_producer_members_at(
        &self,
        generation: i64,
        control: &InvocationControl,
    ) -> Result<SemanticRevisionSelectionState> {
        let producer_id = self.rows.producer_selection_id();
        match self.semantic(
            SemanticOperation::RevisionSelectionAt {
                project_root: self.project_root.clone(),
                selection_id: producer_id.clone(),
                generation,
            },
            control,
        )? {
            SemanticResult::RevisionMembership(Some(state)) => Ok(state),
            _ => Err(invalid_checkpoint(
                format!("{producer_id}@{generation}"),
                "an exact historical producer selection membership",
            )),
        }
    }

    fn validate_project_identity(&self, control: &InvocationControl) -> Result<()> {
        let result = self.semantic(
            SemanticOperation::ProjectIdentity {
                project_root: self.project_root.clone(),
            },
            control,
        )?;
        let SemanticResult::ProjectIdentity(identity) = result else {
            return Err(invalid_checkpoint(
                &self.project_root,
                "a project identity result",
            ));
        };
        if identity.project_id != self.binding.project_id {
            return Err(invalid_checkpoint(
                identity.project_id,
                format!("the bound project identity {}", self.binding.project_id),
            ));
        }
        Ok(())
    }

    fn validate_selection_observation(
        &self,
        observation: &SemanticRevisionSelectionObservation,
        selection_id: &str,
        pinned: bool,
    ) -> Result<()> {
        let selection = &observation.selection;
        if selection.project_root != self.project_root
            || selection.selection_id != selection_id
            || selection.source_context != self.binding.source_context
            || selection.generation < 0
            || observation.expected.generation != selection.generation
            || (pinned && selection.mode != SemanticSelectionMode::Pinned)
        {
            return Err(invalid_checkpoint(
                selection_id,
                "the bound project, source context and expected selection mode/generation",
            ));
        }
        Ok(())
    }

    fn validate_producer_state(
        &self,
        state: &SemanticRevisionSelectionState,
        generation: i64,
    ) -> Result<()> {
        let selection = &state.selection;
        let producer_id = self.rows.producer_selection_id();
        if selection.project_root != self.project_root
            || selection.selection_id != producer_id
            || selection.source_context != self.binding.source_context
            || selection.mode != SemanticSelectionMode::Pinned
            || selection.generation != generation
        {
            return Err(invalid_checkpoint(
                format!("{}@{generation}", selection.selection_id),
                "the bound pinned producer at the requested generation and project route",
            ));
        }
        Ok(())
    }
}

fn same_scoped_members(
    left: &lumvise_db_core::SemanticSelectionExpectation,
    right: &lumvise_db_core::SemanticSelectionExpectation,
) -> bool {
    left.artifact_edits == right.artifact_edits
        && left.records.iter().collect::<BTreeSet<_>>()
            == right.records.iter().collect::<BTreeSet<_>>()
        && left.connection_ids.iter().collect::<BTreeSet<_>>()
            == right.connection_ids.iter().collect::<BTreeSet<_>>()
}
