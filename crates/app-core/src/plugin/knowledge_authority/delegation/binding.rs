//! Execution frames expire immediately with the invocation; the durable grant never widens them.
use super::*;

impl MemoryDelegationAuthority<'_> {
    pub(super) fn bind(
        &self,
        id: &str,
        revision: u64,
        action: DelegatedMemoryAction,
    ) -> Result<Value, HostCapabilityError> {
        let frame = delegation_frame(self.context).ok_or_else(|| unbound(self.context))?;
        let observed = read_delegation(self.authority, id, self.control)?
            .ok_or_else(|| invalid(&json!(id), "an existing owned delegation"))?;
        require_action(&observed, &action)?;
        let setup = self.authority.delegation_setup(
            &observed.executor_agent_id,
            self.context,
            self.control,
        )?;
        if setup && matches!(action, DelegatedMemoryAction::UpdateDestination { .. }) {
            return Err(invalid(&json!(action), "read-only executor preparation"));
        }
        require_bound_revision(&observed, revision, setup)?;
        let mut bindings = self
            .authority
            .delegation_bindings
            .lock()
            .map_err(|_| unbound(self.context))?;
        bindings.retain(|_, binding| binding.deadline > Instant::now());
        bindings.insert(
            frame,
            MemoryDelegationBinding {
                observed: observed.clone(),
                setup,
                deadline: self.context.deadline(),
            },
        );
        drop(bindings);
        let scope = self
            .authority
            .current_delegation(self.semantic, self.context, self.control)?;
        Ok(
            json!({"delegation_id":id,"revision":revision,"document_id":observed.document_id,
            "project_root":scope.space.expect("bound owner").project_root,"action":action}),
        )
    }
}

impl KnowledgeAuthority {
    pub(in crate::plugin::knowledge_authority) fn current_delegation(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        let frame = delegation_frame(context).ok_or_else(|| unbound(context))?;
        let binding = self
            .delegation_bindings
            .lock()
            .map_err(|_| unbound(context))?
            .get(&frame)
            .filter(|binding| binding.deadline > Instant::now())
            .cloned()
            .ok_or_else(|| unbound(context))?;
        let current = read_delegation(self, &binding.observed.delegation_id, control)?
            .ok_or_else(|| unbound(context))?;
        require_bound_revision(&current, binding.observed.revision, binding.setup)?;
        if binding.setup != self.delegation_setup(&current.executor_agent_id, context, control)? {
            return Err(unbound(context));
        }
        self.authorize_delegated_agent(
            &current.executor_agent_id,
            binding.setup,
            semantic,
            context,
            control,
        )?;
        if !binding.setup {
            self.require_delegation_executor(&current.executor_agent_id, context, control)?;
        }
        fresh_owner(self, semantic, current.owner, control)
    }

    pub(in crate::plugin::knowledge_authority) fn is_delegated_executor(
        &self,
        agent: &str,
        control: &InvocationControl,
    ) -> Result<bool, HostCapabilityError> {
        self.identity(control)?;
        Ok(
            read_delegation(self, agent, control)?
                .is_some_and(|row| row.executor_agent_id == agent),
        )
    }
}

fn require_bound_revision(
    delegation: &StoredMemoryDelegation,
    revision: u64,
    setup: bool,
) -> Result<(), HostCapabilityError> {
    if !setup {
        return require_revision(delegation, revision);
    }
    if delegation.revision == revision {
        return Ok(());
    }
    Err(invalid(
        &json!([delegation.delegation_id, revision]),
        "the current setup permission revision",
    ))
}

pub(super) fn require_revision(
    delegation: &StoredMemoryDelegation,
    revision: u64,
) -> Result<(), HostCapabilityError> {
    if delegation.enabled
        && delegation.revision == revision
        && !delegation.executor_observations.is_empty()
    {
        return Ok(());
    }
    Err(invalid(
        &json!([delegation.delegation_id, revision]),
        "the enabled current delegation revision",
    ))
}

fn require_action(
    delegation: &StoredMemoryDelegation,
    action: &DelegatedMemoryAction,
) -> Result<(), HostCapabilityError> {
    if let DelegatedMemoryAction::ReadInput { reference } = action {
        if !delegation.inputs.contains(reference) {
            return Err(invalid(&json!(reference), "an explicitly delegated input"));
        }
    }
    Ok(())
}

pub(super) fn fresh_owner(
    authority: &KnowledgeAuthority,
    semantic: &dyn SemanticPersistence,
    mut scope: KnowledgeAccessContext,
    control: &InvocationControl,
) -> Result<KnowledgeAccessContext, HostCapabilityError> {
    let profile = authority.identity(control)?;
    if scope.storage_authority != profile {
        return Err(invalid(
            &json!(scope.storage_authority),
            "the original profile authority",
        ));
    }
    let current = project_grants(semantic, &profile, control)?;
    scope.project_grants =
        refresh_grants(semantic, &profile, &scope.project_grants, &current, control)?;
    scope.source_projects = scope
        .project_grants
        .iter()
        .map(|grant| grant.project.clone())
        .collect();
    if let Some(project) = scope
        .space
        .as_mut()
        .and_then(|space| space.shared_project.as_mut())
    {
        let grant = scope
            .project_grants
            .iter()
            .find(|grant| grant.project.same_project(project))
            .filter(|grant| grant.writable)
            .ok_or_else(|| invalid(&json!(project), "current shared-memory write permission"))?;
        *project = grant.project.clone();
        scope.active_project = Some(project.clone());
    }
    Ok(scope)
}

fn refresh_grants(
    semantic: &dyn SemanticPersistence,
    profile: &str,
    originals: &[KnowledgeProjectGrant],
    current: &[KnowledgeProjectGrant],
    control: &InvocationControl,
) -> Result<Vec<KnowledgeProjectGrant>, HostCapabilityError> {
    originals
        .iter()
        .map(|original| {
            // Catalog discovery may lag an authorized MCP binding; always ask the adapter before denying it.
            let mut grant = match current
                .iter()
                .find(|grant| grant.project.same_project(&original.project))
            {
                Some(grant) => grant.clone(),
                None => project_access::resolve_grant(
                    semantic,
                    &original.project.project_root,
                    profile,
                    control,
                )?,
            };
            if !grant.project.same_project(&original.project) {
                return Err(invalid(
                    &json!(grant.project),
                    "the original delegated storage authority and lineage",
                ));
            }
            grant.writable &= original.writable;
            Ok(grant)
        })
        .collect()
}
