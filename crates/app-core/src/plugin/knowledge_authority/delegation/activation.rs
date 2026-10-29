//! A pre-existing Agent ID is not a grant: activation binds accepted executor control facts.
use super::*;

impl MemoryDelegationAuthority<'_> {
    pub(super) fn activate(
        &self,
        id: &str,
        revision: u64,
        observations: Vec<agent::AgentScopeObservation>,
    ) -> Result<Value, HostCapabilityError> {
        let owner = self.owner()?;
        let mut current = read_delegation(self.authority, id, self.control)?
            .ok_or_else(|| unbound(self.context))?;
        require_activation(&owner, &current, revision, &observations)?;
        let previous = json!(current);
        for fact in &observations {
            fact.verify(self.authority.relational.as_ref(), self.control)?;
        }
        current.executor_observations = observations;
        storage::replace(
            self.authority.relational.as_ref(),
            &format!("delegation:{id}"),
            Some(previous),
            json!(current),
            self.control,
        )?;
        Ok(json!({"delegation":delegation_view(current)}))
    }
}

fn require_activation(
    owner: &KnowledgeAccessContext,
    current: &StoredMemoryDelegation,
    revision: u64,
    observations: &[agent::AgentScopeObservation],
) -> Result<(), HostCapabilityError> {
    if delegation_id(owner, &current.document_id)? != current.delegation_id
        || current.revision != revision
        || observations.is_empty()
        || observations.iter().any(|fact| !fact.is_present())
    {
        return Err(invalid(
            &json!(current.delegation_id),
            "the owned current delegation and accepted executor configuration observations",
        ));
    }
    if !current.executor_observations.is_empty()
        && json!(current.executor_observations) != json!(observations)
    {
        return Err(invalid(
            &json!(current.delegation_id),
            "a new permission revision before changing the executor configuration",
        ));
    }
    Ok(())
}
