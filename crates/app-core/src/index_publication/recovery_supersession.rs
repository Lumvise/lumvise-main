//! Replaces a recovery-blocked stale request only after proving its exact operation unaccepted.
use super::*;
use lumvise_db_core::{SemanticRecordKind, SemanticRevisionRecordHeader};
use std::collections::BTreeMap;

impl IndexPublicationJournal {
    pub(super) fn supersede_stale_recovered_pending(
        &self,
        publication: &FrozenIndexPublication,
        unavailable: &[SemanticRevisionRecordHeader],
        control: &InvocationControl,
    ) -> Result<bool> {
        let headers = source_unavailable_headers(unavailable, &self.binding.provider_instance_id);
        if headers.is_empty() {
            return Ok(false);
        }
        let Some((commit, observed)) = self.stale_recovery_observation(publication, control)?
        else {
            return Ok(false);
        };
        self.commit_stale_recovery_supersession(publication, headers, (commit, observed), control)?;
        Ok(true)
    }

    fn commit_stale_recovery_supersession(
        &self,
        publication: &FrozenIndexPublication,
        blocked_headers: Vec<SemanticRevisionRecordHeader>,
        observation: (i64, super::recovery_observation::RecoveredSourceObservation),
        control: &InvocationControl,
    ) -> Result<()> {
        let (commit, scoped_conflict) = observation;
        self.commit_source_supersession(
            publication,
            RecoveredSourceSupersession {
                operation_id: publication.operation_id.clone(),
                frozen_digest: pending::publication_digest(publication)?,
                inspected_commit_version: commit,
                blocked_headers,
                scoped_conflict: Some(scoped_conflict),
            },
            control,
        )
    }

    fn stale_recovery_observation(
        &self,
        publication: &FrozenIndexPublication,
        control: &InvocationControl,
    ) -> Result<Option<(i64, super::recovery_observation::RecoveredSourceObservation)>> {
        let Some(commit) = self.unaccepted_recovery_commit(&publication.operation_id, control)?
        else {
            return Ok(None);
        };
        let observed = self.observe_recovered_scope(&publication.partition(), control)?;
        if observed.matches_frozen(publication.observation()?) {
            return Ok(None);
        }
        self.require_same_unaccepted_commit(&publication.operation_id, commit, control)?;
        Ok(Some((commit, observed)))
    }

    fn require_same_unaccepted_commit(
        &self,
        operation: &str,
        commit: i64,
        control: &InvocationControl,
    ) -> Result<()> {
        if self.unaccepted_recovery_commit(operation, control)? != Some(commit) {
            return Err(invalid_checkpoint(
                operation,
                "the same unaccepted publication and guarded recovery commit",
            ));
        }
        Ok(())
    }
}

fn source_unavailable_headers(
    headers: &[SemanticRevisionRecordHeader],
    provider: &str,
) -> Vec<SemanticRevisionRecordHeader> {
    headers
        .iter()
        .filter(|header| {
            header.unavailable.is_some()
                && header.record.kind == SemanticRecordKind::Element
                && header
                    .element_scope
                    .as_ref()
                    .is_some_and(|scope| scope.provider_instance_id.as_deref() == Some(provider))
        })
        .map(|header| (header.record.clone(), header.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}
