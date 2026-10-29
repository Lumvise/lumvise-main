//! Authorizes opaque future Agent owner facts before setup side effects. The original
//! frozen intent survives a crash before either owner's acknowledgement.
use super::*;
use lumvise_plugin_runtime::PluginInvocationClass;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::plugin::knowledge_authority) struct MemoryExecutorPreparation {
    digest: String,
    pub(super) pending: Vec<agent::AgentScopeObservation>,
    accepted: Vec<agent::AgentScopeObservation>,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::plugin::knowledge_authority) enum ExecutorPreparationRequest {
    Get {
        agent_id: String,
        revision: u64,
    },
    Save {
        agent_id: String,
        revision: u64,
        preparation: MemoryExecutorPreparation,
    },
    Pending {},
}

impl KnowledgeAuthority {
    pub(in crate::plugin::knowledge_authority) fn executor_preparation(
        &self,
        plugin: &str,
        request: ExecutorPreparationRequest,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        if plugin != "builtin.agents"
            || context
                .plugin_chain()
                .last()
                .is_none_or(|frame| frame.plugin_id != plugin)
        {
            return Err(unbound(context));
        }
        match request {
            ExecutorPreparationRequest::Get { agent_id, revision } => {
                let saved = self
                    .executor_preparation_owner(&agent_id, revision, semantic, context, control)?;
                Ok(json!({"preparation":saved.executor_preparation}))
            }
            ExecutorPreparationRequest::Save {
                agent_id,
                revision,
                preparation,
            } => self.save_executor_preparation(
                &agent_id,
                revision,
                preparation,
                semantic,
                context,
                control,
            ),
            ExecutorPreparationRequest::Pending {} => {
                self.pending_executor_preparations(semantic, context, control)
            }
        }
    }

    fn save_executor_preparation(
        &self,
        agent: &str,
        revision: u64,
        preparation: MemoryExecutorPreparation,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        let mut saved =
            self.executor_preparation_owner(agent, revision, semantic, context, control)?;
        preparation.validate()?;
        if let Some(original) = &saved.executor_preparation {
            if original.digest == preparation.digest {
                return Ok(json!({"preparation":original}));
            }
            return Err(invalid(
                &json!(preparation.digest),
                "the originally authorized executor configuration",
            ));
        }
        let previous = json!(saved);
        saved.executor_preparation = Some(preparation.clone());
        storage::replace(
            self.relational.as_ref(),
            &format!("delegation:{agent}"),
            Some(previous),
            json!(saved),
            control,
        )?;
        Ok(json!({"preparation":preparation}))
    }

    fn executor_preparation_owner(
        &self,
        agent: &str,
        revision: u64,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<StoredMemoryDelegation, HostCapabilityError> {
        if !self.delegation_setup(agent, context, control)? {
            return Err(unbound(context));
        }
        self.authorize_delegated_agent(agent, true, semantic, context, control)?;
        let saved = read_delegation(self, agent, control)?.ok_or_else(|| unbound(context))?;
        if saved.revision != revision {
            return Err(unbound(context));
        }
        Ok(saved)
    }

    fn pending_executor_preparations(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        if context.class() != PluginInvocationClass::Background || context.scope_id().is_some() {
            return Err(unbound(context));
        }
        require_autonomous_executor_origin(context)?;
        self.identity(control)?;
        let mut pending = Vec::new();
        let mut blocked = Vec::new();
        for value in storage::list_prefix(self.relational.as_ref(), "delegation:", control)? {
            let saved: StoredMemoryDelegation = serde_json::from_value(value.clone())
                .map_err(|_| invalid(&value, "a stored delegation"))?;
            if saved.executor_preparation.is_none() || !saved.executor_observations.is_empty() {
                continue;
            }
            match self.recoverable_preparation(saved.clone(), semantic, control) {
                Ok(Some(preparation)) => pending.push(json!({"agent_id":saved.executor_agent_id,"revision":saved.revision,"preparation":preparation})),
                Ok(None) => {},
                Err(error) => blocked.push(json!({"agent_id":saved.executor_agent_id,"error":error.to_string()})),
            }
        }
        Ok(json!({"pending":pending,"blocked":blocked}))
    }

    fn recoverable_preparation(
        &self,
        saved: StoredMemoryDelegation,
        semantic: &dyn SemanticPersistence,
        control: &InvocationControl,
    ) -> Result<Option<MemoryExecutorPreparation>, HostCapabilityError> {
        let saved = self.reconcile_executor_acceptance(saved, semantic, control)?;
        Ok(saved
            .executor_observations
            .is_empty()
            .then_some(saved.executor_preparation)
            .flatten())
    }

    pub(super) fn reconcile_executor_acceptance(
        &self,
        mut saved: StoredMemoryDelegation,
        semantic: &dyn SemanticPersistence,
        control: &InvocationControl,
    ) -> Result<StoredMemoryDelegation, HostCapabilityError> {
        binding::fresh_owner(self, semantic, saved.owner.clone(), control)?;
        if !saved.executor_observations.is_empty() {
            return Ok(saved);
        }
        let Some(prepared) = &saved.executor_preparation else {
            return Ok(saved);
        };
        // Missing acceptance leaves the original intent pending; it never authorizes a run.
        for fact in &prepared.accepted {
            if !fact.matches(self.relational.as_ref(), control)? {
                return Ok(saved);
            }
        }
        let previous = json!(saved);
        saved.executor_observations = prepared.accepted.clone();
        storage::replace(
            self.relational.as_ref(),
            &format!("delegation:{}", saved.delegation_id),
            Some(previous),
            json!(saved),
            control,
        )?;
        Ok(saved)
    }
}

impl MemoryExecutorPreparation {
    fn validate(&self) -> Result<(), HostCapabilityError> {
        if self.digest.trim().is_empty()
            || self.pending.is_empty()
            || self.accepted.is_empty()
            || self
                .pending
                .iter()
                .chain(&self.accepted)
                .any(|fact| !fact.is_present())
        {
            return Err(invalid(
                &json!(self),
                "a nonempty intent digest and present pending/accepted owner facts",
            ));
        }
        Ok(())
    }
}
