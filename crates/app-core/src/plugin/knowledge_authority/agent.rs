//! Agents supplies grant facts; opaque owner row observations fence those facts.
//! Bindings belong to one trusted runtime frame, never to model request IDs.
use super::*;
use std::time::Instant;

mod grants;
mod run_provenance;

const AGENTS: &str = "builtin.agents";

#[cfg(test)]
mod tests;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentScopeEvidence {
    agent_id: String,
    grants: Vec<AgentScopeGrant>,
    observations: Vec<AgentScopeObservation>,
    run_id: Option<String>,
    purpose: Option<AgentScopePurpose>,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum AgentScopePurpose {
    Autonomous,
    OwnerControl,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentScopeGrant {
    project_root: String,
    writable: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentScopeObservation {
    table: String,
    row_key: String,
    expected: Option<Value>,
}

#[derive(Clone)]
pub(super) struct AgentScopeBinding {
    evidence: AgentScopeEvidence,
    deadline: Instant,
    sources: Vec<KnowledgeProjectIdentity>,
}

pub(super) fn agent_frame(context: &PluginInvocationContext) -> Option<u64> {
    context
        .plugin_chain()
        .iter()
        .rev()
        .find(|frame| frame.plugin_id == AGENTS)
        .map(|frame| frame.binding_id)
}

fn has_memory_origin(context: &PluginInvocationContext) -> bool {
    matches!(
        context.scope_id(),
        Some("assistant_session" | lumvise_contracts::KNOWLEDGE_MCP_SCOPE)
    )
}

impl KnowledgeAuthority {
    pub(super) fn delegation_setup(
        &self,
        executor: &str,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<bool, HostCapabilityError> {
        let binding = self.agent_binding(
            agent_frame(context).ok_or_else(|| unbound(context))?,
            context,
        )?;
        self.check_agent_evidence(&binding.evidence, control)?;
        if binding.evidence.agent_id != executor {
            return Err(unbound(context));
        }
        Ok(binding.evidence.purpose == Some(AgentScopePurpose::OwnerControl))
    }

    pub(super) fn require_delegation_executor(
        &self,
        executor: &str,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        delegation::require_autonomous_executor_origin(context)?;
        let binding = self.agent_binding(
            agent_frame(context).ok_or_else(|| unbound(context))?,
            context,
        )?;
        self.check_agent_evidence(&binding.evidence, control)?;
        if binding.evidence.agent_id != executor
            || binding.evidence.purpose != Some(AgentScopePurpose::Autonomous)
            || binding
                .evidence
                .observations
                .first()
                .is_none_or(|fact| fact.expected.is_none())
        {
            return Err(invalid(
                &json!(executor),
                "the delegation's existing authenticated autonomous executor",
            ));
        }
        Ok(())
    }

    pub(super) fn bind_agent(
        &self,
        plugin: &str,
        mut evidence: AgentScopeEvidence,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        self.preserve_agent_purpose(&mut evidence, context)?;
        self.reuse_run_observation(&mut evidence, context)?;
        let frame = trusted_frame(plugin, &evidence, context)?;
        self.check_agent_evidence(&evidence, control)?;
        let grants = self.agent_grants(&evidence, semantic, context, control)?;
        let sources = self.binding_sources(&evidence, &grants, semantic, context, control)?;
        self.install_agent_binding(frame, evidence, sources, context)
    }

    fn install_agent_binding(
        &self,
        frame: u64,
        evidence: AgentScopeEvidence,
        mut sources: Vec<KnowledgeProjectIdentity>,
        context: &PluginInvocationContext,
    ) -> Result<(), HostCapabilityError> {
        let mut bindings = self.agent_bindings.lock().map_err(|_| unbound(context))?;
        bindings.retain(|_, binding| binding.deadline > Instant::now());
        if let Some(previous) = bindings
            .get(&frame)
            .filter(|old| old.evidence.agent_id == evidence.agent_id)
        {
            sources.extend(previous.sources.clone());
        }
        sources.sort();
        sources.dedup();
        bindings.insert(
            frame,
            AgentScopeBinding {
                evidence,
                deadline: context.deadline(),
                sources,
            },
        );
        Ok(())
    }

    fn preserve_agent_purpose(
        &self,
        evidence: &mut AgentScopeEvidence,
        context: &PluginInvocationContext,
    ) -> Result<(), HostCapabilityError> {
        if evidence.purpose.is_some() {
            return Ok(());
        }
        let bindings = self.agent_bindings.lock().map_err(|_| unbound(context))?;
        evidence.purpose = agent_frame(context)
            .and_then(|frame| bindings.get(&frame))
            .filter(|old| old.evidence.agent_id == evidence.agent_id)
            .and_then(|old| old.evidence.purpose)
            .or(Some(AgentScopePurpose::Autonomous));
        Ok(())
    }

    fn reuse_run_observation(
        &self,
        evidence: &mut AgentScopeEvidence,
        context: &PluginInvocationContext,
    ) -> Result<(), HostCapabilityError> {
        if context.scope_id() != Some("agent_run") || evidence.run_id.is_some() {
            return Ok(());
        }
        let old = self.agent_binding(
            agent_frame(context).ok_or_else(|| unbound(context))?,
            context,
        )?;
        if old.evidence.agent_id != evidence.agent_id {
            return Err(unbound(context));
        }
        evidence.run_id = old.evidence.run_id;
        for observation in old.evidence.observations {
            if !evidence
                .observations
                .iter()
                .any(|item| item.table == observation.table && item.row_key == observation.row_key)
            {
                evidence.observations.push(observation);
            }
        }
        Ok(())
    }

    pub(super) fn current_agent(
        &self,
        frame: u64,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        let binding = self.agent_binding(frame, context)?;
        self.check_agent_evidence(&binding.evidence, control)?;
        self.authorize_delegated_agent(
            &binding.evidence.agent_id,
            binding.evidence.purpose == Some(AgentScopePurpose::OwnerControl),
            semantic,
            context,
            control,
        )?;
        let grants = self.agent_grants(&binding.evidence, semantic, context, control)?;
        if binding.sources.iter().any(|project| {
            !grants
                .iter()
                .any(|grant| grant.project.same_project(project))
        }) {
            return Err(invalid(
                &json!(binding.evidence.agent_id),
                "a fresh run after source grant revocation",
            ));
        }
        let authority = self.identity(control)?;
        Ok(agent_scope(&binding, authority, grants))
    }

    fn agent_binding(
        &self,
        frame: u64,
        context: &PluginInvocationContext,
    ) -> Result<AgentScopeBinding, HostCapabilityError> {
        self.agent_bindings
            .lock()
            .map_err(|_| unbound(context))?
            .get(&frame)
            .filter(|binding| binding.deadline > Instant::now())
            .cloned()
            .ok_or_else(|| unbound(context))
    }

    fn check_agent_evidence(
        &self,
        evidence: &AgentScopeEvidence,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        for observation in &evidence.observations {
            observation.verify(self.relational.as_ref(), control)?;
        }
        Ok(())
    }

    pub(super) fn observe_agent_sources(
        &self,
        context: &PluginInvocationContext,
        sources: &[KnowledgeProjectIdentity],
    ) -> Result<(), HostCapabilityError> {
        let frame = agent_frame(context).ok_or_else(|| unbound(context))?;
        let mut bindings = self.agent_bindings.lock().map_err(|_| unbound(context))?;
        let binding = bindings.get_mut(&frame).ok_or_else(|| unbound(context))?;
        for source in sources {
            if !binding.sources.contains(source) {
                binding.sources.push(source.clone());
            }
        }
        Ok(())
    }
}

impl AgentScopeObservation {
    pub(super) fn is_present(&self) -> bool {
        self.expected.is_some()
    }

    pub(super) fn matches(
        &self,
        relational: &dyn RelationalPersistence,
        control: &InvocationControl,
    ) -> Result<bool, HostCapabilityError> {
        storage::owner_observation_matches(
            relational,
            AGENTS,
            &self.table,
            &self.row_key,
            self.expected.as_ref(),
            control,
        )
    }

    pub(super) fn verify(
        &self,
        relational: &dyn RelationalPersistence,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        storage::require_owner_observation(
            relational,
            AGENTS,
            &self.table,
            &self.row_key,
            self.expected.as_ref(),
            control,
        )
    }
}

fn trusted_frame(
    plugin: &str,
    evidence: &AgentScopeEvidence,
    context: &PluginInvocationContext,
) -> Result<u64, HostCapabilityError> {
    if plugin != AGENTS
        || context
            .plugin_chain()
            .last()
            .is_none_or(|frame| frame.plugin_id != AGENTS)
        || !(1..=64).contains(&evidence.agent_id.len())
        || !evidence
            .agent_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || evidence.observations.is_empty()
    {
        return Err(invalid(
            &json!(evidence.agent_id),
            "trusted Agents frame with owner observations",
        ));
    }
    if context.scope_id() == Some("agent_run")
        && (context.owner_id() != AGENTS
            || evidence.purpose != Some(AgentScopePurpose::Autonomous)
            || evidence.run_id.as_deref() != context.session_id()
            || evidence.observations.len() < 2)
    {
        return Err(unbound(context));
    }
    agent_frame(context).ok_or_else(|| unbound(context))
}

fn agent_scope(
    binding: &AgentScopeBinding,
    authority: String,
    grants: Vec<KnowledgeProjectGrant>,
) -> KnowledgeAccessContext {
    let agent = &binding.evidence.agent_id;
    let space = agent_space(&authority, agent);
    KnowledgeAccessContext {
        principal_kind: KnowledgePrincipalKind::Agent,
        principal_id: agent.clone(),
        security_domain: authority.clone(),
        storage_authority: authority,
        project_grants: grants,
        space: Some(space),
        active_project: None,
        source_projects: binding.sources.clone(),
    }
}

pub(super) fn agent_space(authority: &str, agent: &str) -> KnowledgeSpaceDescriptor {
    KnowledgeSpaceDescriptor {
        space_id: format!("agent:{authority}:{agent}"),
        space_kind: KnowledgeSpaceKind::Agent,
        project_root: format!("agent-space://{agent}"),
        principal_id: agent.into(),
        security_domain: authority.into(),
        storage_authority: authority.into(),
        shared_project: None,
        revision: 1,
    }
}
