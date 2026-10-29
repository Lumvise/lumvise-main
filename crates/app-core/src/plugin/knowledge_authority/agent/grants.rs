//! Resolves owner facts against the originating caller without owning Agent definitions.
use super::*;

impl KnowledgeAuthority {
    pub(super) fn agent_grants(
        &self,
        evidence: &AgentScopeEvidence,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<Vec<KnowledgeProjectGrant>, HostCapabilityError> {
        if self.is_delegated_executor(&evidence.agent_id, control)? {
            return Ok(Vec::new());
        }
        let mut grants = project_grants(semantic, &self.identity(control)?, control)?;
        if evidence.purpose != Some(AgentScopePurpose::OwnerControl) {
            grants.retain_mut(|grant| evidence.intersect_grant(grant));
        }
        if has_memory_origin(context) {
            let origin = self.current_origin(semantic, context, control)?;
            grants.retain_mut(|grant| intersect_origin_grant(grant, &origin));
        }
        Ok(grants)
    }

    pub(in crate::plugin::knowledge_authority) fn authorize_agent_grants(
        &self,
        plugin: &str,
        proposed: &[AgentScopeGrant],
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        self.require_agent_control(plugin, semantic, context, control)?;
        let origin = has_memory_origin(context)
            .then(|| self.current_origin(semantic, context, control))
            .transpose()?;
        for grant in proposed {
            grant.require_assignment(origin.as_ref())?;
        }
        Ok(())
    }

    fn require_agent_control(
        &self,
        plugin: &str,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let frame = agent_frame(context).ok_or_else(|| unbound(context))?;
        let binding = self.agent_binding(frame, context)?;
        trusted_frame(plugin, &binding.evidence, context)?;
        self.check_agent_evidence(&binding.evidence, control)?;
        if binding.evidence.purpose != Some(AgentScopePurpose::OwnerControl) {
            return Err(invalid(
                &json!(binding.evidence.agent_id),
                "owner control for Agent grant assignment",
            ));
        }
        self.authorize_delegated_agent(
            &binding.evidence.agent_id,
            true,
            semantic,
            context,
            control,
        )?;
        Ok(())
    }
}

impl AgentScopeEvidence {
    fn intersect_grant(&self, grant: &mut KnowledgeProjectGrant) -> bool {
        let writable = self
            .grants
            .iter()
            .filter(|owner| owner.project_root == grant.project.project_root)
            .map(|owner| owner.writable)
            .reduce(|left, right| left || right);
        let Some(writable) = writable else {
            return false;
        };
        grant.writable &= writable;
        true
    }
}

impl AgentScopeGrant {
    fn require_assignment(
        &self,
        origin: Option<&KnowledgeAccessContext>,
    ) -> Result<(), HostCapabilityError> {
        let root = self.project_root.trim_end_matches('/');
        let authorized = origin.is_none_or(|scope| {
            scope.project_grants.iter().any(|parent| {
                parent.project.project_root == root && (!self.writable || parent.writable)
            })
        });
        if root.is_empty() || is_memory_root(root) || !authorized {
            return Err(invalid(
                &json!(self.project_root),
                "a source project within the caller's assignment rights",
            ));
        }
        Ok(())
    }
}

fn intersect_origin_grant(
    grant: &mut KnowledgeProjectGrant,
    origin: &KnowledgeAccessContext,
) -> bool {
    let Some(parent) = origin
        .project_grants
        .iter()
        .find(|parent| parent.project == grant.project)
    else {
        return false;
    };
    grant.writable &= parent.writable;
    true
}
