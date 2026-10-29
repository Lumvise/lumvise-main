//! Only the owning Knowledge frame can prepare private executor controls. Ordinary Agent
//! calls cannot use an executor's opaque identity to inspect its Mission/control history.
use super::*;
use lumvise_contracts::KnowledgePrincipalKind;
use lumvise_plugin_runtime::PluginInvocationClass;

#[derive(Clone)]
pub(in crate::plugin::knowledge_authority) struct MemoryExecutorSetupBinding {
    delegation_id: String,
    revision: u64,
    origin: KnowledgePrincipalKind,
    space_id: String,
    deadline: Instant,
}

impl MemoryDelegationAuthority<'_> {
    pub(super) fn prepare_executor(
        &self,
        id: &str,
        revision: u64,
    ) -> Result<Value, HostCapabilityError> {
        let owner = self.owner()?;
        let saved = read_delegation(self.authority, id, self.control)?
            .ok_or_else(|| unbound(self.context))?;
        if saved.revision != revision || delegation_id(&owner, &saved.document_id)? != id {
            return Err(invalid(
                &json!([id, revision]),
                "the owner's current permission revision",
            ));
        }
        let frame = self
            .context
            .plugin_chain()
            .last()
            .expect("trusted Knowledge frame")
            .binding_id;
        let setup = MemoryExecutorSetupBinding {
            delegation_id: id.into(),
            revision,
            origin: owner.principal_kind,
            space_id: owner.space.expect("owned space").space_id,
            deadline: self.context.deadline(),
        };
        let mut setups = self
            .authority
            .executor_setups
            .lock()
            .map_err(|_| unbound(self.context))?;
        setups.retain(|_, setup| setup.deadline > Instant::now());
        setups.insert(frame, setup);
        Ok(
            json!({"delegation_id":id,"revision":revision,"executor_agent_id":saved.executor_agent_id}),
        )
    }
}

impl KnowledgeAuthority {
    pub(in crate::plugin::knowledge_authority) fn authorize_delegated_agent(
        &self,
        agent: &str,
        owner_control: bool,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let Some(saved) = read_delegation(self, agent, control)? else {
            return Ok(());
        };
        let saved = if owner_control {
            saved
        } else {
            self.reconcile_executor_acceptance(saved, semantic, control)?
        };
        if owner_control {
            self.require_executor_setup(&saved, semantic, context, control)?;
        } else {
            require_autonomous_executor_origin(context)?;
            binding::require_revision(&saved, saved.revision)?;
            for fact in &saved.executor_observations {
                fact.verify(self.relational.as_ref(), control)?;
            }
        }
        binding::fresh_owner(self, semantic, saved.owner, control)?;
        Ok(())
    }

    fn require_executor_setup(
        &self,
        saved: &StoredMemoryDelegation,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if context.class() == PluginInvocationClass::Background && context.scope_id().is_none() {
            require_autonomous_executor_origin(context)?;
            let prepared = saved
                .executor_preparation
                .as_ref()
                .ok_or_else(|| unbound(context))?;
            for fact in &prepared.pending {
                fact.verify(self.relational.as_ref(), control)?;
            }
            return Ok(());
        }
        let setups = self.executor_setups.lock().map_err(|_| unbound(context))?;
        let setup = context
            .plugin_chain()
            .iter()
            .rev()
            .filter(|frame| frame.plugin_id == KNOWLEDGE)
            .find_map(|frame| setups.get(&frame.binding_id))
            .cloned()
            .ok_or_else(|| unbound(context))?;
        drop(setups);
        if setup.deadline <= Instant::now()
            || setup.delegation_id != saved.delegation_id
            || setup.revision != saved.revision
        {
            return Err(unbound(context));
        }
        self.require_setup_origin(&setup, semantic, context, control)
    }

    fn require_setup_origin(
        &self,
        setup: &MemoryExecutorSetupBinding,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if setup.origin == KnowledgePrincipalKind::Human
            && context.is_human_interaction()
            && context.scope_id().is_none()
        {
            return Ok(());
        }
        let origin = self.current_origin(semantic, context, control)?;
        if origin
            .space
            .is_some_and(|space| space.space_id == setup.space_id)
        {
            return Ok(());
        }
        Err(unbound(context))
    }
}

pub(in crate::plugin::knowledge_authority) fn require_autonomous_executor_origin(
    context: &PluginInvocationContext,
) -> Result<(), HostCapabilityError> {
    if context.scope_id() == Some("agent_run") {
        return Ok(());
    }
    if context.class() == PluginInvocationClass::Background
        && context.scope_id().is_none()
        && context
            .plugin_chain()
            .first()
            .is_some_and(|frame| frame.plugin_id == "builtin.agents")
    {
        return Ok(());
    }
    Err(unbound(context))
}
