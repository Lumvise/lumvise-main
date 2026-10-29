//! Retains supplied run context so reconnecting cannot erase revoked project provenance.
use super::*;

impl KnowledgeAuthority {
    pub(super) fn binding_sources(
        &self,
        evidence: &AgentScopeEvidence,
        grants: &[KnowledgeProjectGrant],
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<Vec<KnowledgeProjectIdentity>, HostCapabilityError> {
        // The host-owned delegation conservatively retains this executor's source restrictions.
        // Its private controls are separately gated; generic Agent grants never widen them.
        if self.is_delegated_executor(&evidence.agent_id, control)? {
            return Ok(Vec::new());
        }
        let mut sources = self.agent_run_sources(evidence, grants, control)?;
        if has_memory_origin(context) {
            sources.extend(
                self.current_origin(semantic, context, control)?
                    .source_projects,
            );
        }
        Ok(sources)
    }
    pub(super) fn agent_run_sources(
        &self,
        evidence: &AgentScopeEvidence,
        grants: &[KnowledgeProjectGrant],
        control: &InvocationControl,
    ) -> Result<Vec<KnowledgeProjectIdentity>, HostCapabilityError> {
        let Some(run) = &evidence.run_id else {
            return Ok(Vec::new());
        };
        let key = format!("agent-run:{}:{run}", evidence.agent_id);
        let _guard = self
            .binding_write
            .lock()
            .map_err(|_| invalid(&json!(run), "available run scope lock"))?;
        let previous = storage::read(self.relational.as_ref(), &key, control)?;
        let sources = next_run_sources(previous.as_ref(), grants, run)?;
        let next = json!(sources);
        if previous.as_ref() != Some(&next) {
            storage::replace(self.relational.as_ref(), &key, previous, next, control)?;
        }
        Ok(sources)
    }
}

fn next_run_sources(
    previous: Option<&Value>,
    grants: &[KnowledgeProjectGrant],
    run: &str,
) -> Result<Vec<KnowledgeProjectIdentity>, HostCapabilityError> {
    let mut sources: Vec<KnowledgeProjectIdentity> = previous
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| invalid(&json!(run), "stored run project provenance"))?
        .unwrap_or_default();
    if sources.iter().any(|project| {
        !grants
            .iter()
            .any(|grant| grant.project.same_project(project))
    }) {
        return Err(invalid(
            &json!(run),
            "a new run after supplied source context was revoked",
        ));
    }
    sources.extend(grants.iter().map(|grant| grant.project.clone()));
    sources.sort();
    sources.dedup();
    Ok(sources)
}
