//! Conservatively retains project provenance supplied to each live conversation.
use super::*;
use lumvise_contracts::KnowledgeDocumentAccess;

impl KnowledgeAuthority {
    pub(in crate::plugin) fn authorize_source(
        &self,
        context: &PluginInvocationContext,
        semantic: &dyn SemanticPersistence,
        root: &str,
        writable: bool,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if is_memory_root(root) {
            return Err(invalid(&json!(root), "ordinary source project"));
        }
        if agent::agent_frame(context).is_none()
            && !matches!(
                context.scope_id(),
                Some("assistant_session" | lumvise_contracts::KNOWLEDGE_MCP_SCOPE | "agent_run")
            )
        {
            return Ok(());
        }
        let scope = self.current(semantic, context, control)?;
        if !scope
            .project_grants
            .iter()
            .any(|grant| grant.project.project_root == root && (!writable || grant.writable))
        {
            return Err(invalid(&json!(root), "current source project grant"));
        }
        self.observe_project(context, root, Some(&scope), control)
    }

    pub(super) fn observe_project(
        &self,
        context: &PluginInvocationContext,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let Some(scope) = scope else {
            return Ok(());
        };
        let project = scope
            .project_grants
            .iter()
            .find(|grant| grant.project.project_root == root)
            .ok_or_else(|| invalid(&json!(root), "current project grant"))?;
        self.record_sources(
            context,
            std::slice::from_ref(&project.project),
            scope,
            control,
        )
    }

    pub(super) fn observe_access(
        &self,
        context: &PluginInvocationContext,
        value: &Value,
        scope: &KnowledgeAccessContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        let access: KnowledgeDocumentAccess = serde_json::from_value(value.clone())
            .map_err(|_| invalid(value, "stored document provenance"))?;
        if scope
            .space
            .as_ref()
            .is_none_or(|space| space.space_id != access.space_id)
        {
            return Err(invalid(value, "the bound document space"));
        }
        self.record_sources(context, &access.required_projects, scope, control)
    }

    fn record_sources(
        &self,
        context: &PluginInvocationContext,
        sources: &[KnowledgeProjectIdentity],
        scope: &KnowledgeAccessContext,
        control: &InvocationControl,
    ) -> Result<(), HostCapabilityError> {
        if sources.iter().any(|project| {
            !scope
                .project_grants
                .iter()
                .any(|grant| grant.project.same_project(project))
        }) {
            return Err(invalid(
                &json!(sources),
                "current grants for document provenance",
            ));
        }
        if super::delegation::delegation_frame(context).is_some() {
            return Ok(());
        }
        // Every MCP memory document already carries its sole authorized source project.
        if matches!(
            scope.principal_kind,
            KnowledgePrincipalKind::Mcp | KnowledgePrincipalKind::Human
        ) {
            return Ok(());
        }
        if scope.principal_kind == KnowledgePrincipalKind::Agent {
            self.observe_agent_sources(context, sources)?;
            if context.scope_id() != Some("assistant_session") {
                return Ok(());
            }
        }
        let key = binding_key(context.session_id().ok_or_else(|| unbound(context))?);
        let _guard = self
            .binding_write
            .lock()
            .map_err(|_| invalid(&json!(key), "available session binding lock"))?;
        let previous = storage::read(self.relational.as_ref(), &key, control)?
            .ok_or_else(|| unbound(context))?;
        let mut binding = decode_binding(&previous)?;
        if !binding.active
            || binding.incarnation != self.incarnation
            || context.scope_epoch() != Some(binding.session_epoch)
        {
            return Err(unbound(context));
        }
        let before = binding.source_projects.len();
        for project in sources {
            if !binding.source_projects.contains(project) {
                binding.source_projects.push(project.clone());
            }
        }
        if binding.source_projects.len() == before {
            return Ok(());
        }
        storage::replace(
            self.relational.as_ref(),
            &key,
            Some(previous),
            json!(binding),
            control,
        )
    }
}
