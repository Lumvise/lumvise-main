//! Native human inspection reuses one selected owner space. No HTTP/MCP string grants this origin.
use super::*;
use lumvise_contracts::KnowledgeSpaceInspection;
use std::time::Instant;

#[derive(Clone)]
pub(super) struct HumanInspectionBinding {
    space: KnowledgeSpaceDescriptor,
    deadline: Instant,
}

pub(super) fn inspection_frame(context: &PluginInvocationContext) -> Option<u64> {
    if !context.is_human_interaction()
        || context.scope_id().is_some()
        || agent::agent_frame(context).is_some()
    {
        return None;
    }
    context
        .plugin_chain()
        .iter()
        .find(|frame| {
            frame.plugin_id == "builtin.knowledge" && frame.export_id == "inspect_document_space"
        })
        .map(|frame| frame.binding_id)
}

impl KnowledgeAuthority {
    pub(super) fn bind_inspection(
        &self,
        plugin: &str,
        selection: KnowledgeSpaceInspection,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<Value, HostCapabilityError> {
        if plugin != "builtin.knowledge" {
            return Err(unbound(context));
        }
        let frame = inspection_frame(context).ok_or_else(|| unbound(context))?;
        let authority = self.identity(control)?;
        let space = self.inspection_space(selection, &authority, semantic, control)?;
        let mut bindings = self
            .inspection_bindings
            .lock()
            .map_err(|_| unbound(context))?;
        bindings.retain(|_, entry| entry.deadline > Instant::now());
        bindings.insert(
            frame,
            HumanInspectionBinding {
                space,
                deadline: context.deadline(),
            },
        );
        drop(bindings);
        let scope = self.current_inspection(semantic, context, control)?;
        Ok(json!({"space":scope.space,"writable":super::project_access::memory_writable(&scope)}))
    }

    fn inspection_space(
        &self,
        selection: KnowledgeSpaceInspection,
        authority: &str,
        semantic: &dyn SemanticPersistence,
        control: &InvocationControl,
    ) -> Result<KnowledgeSpaceDescriptor, HostCapabilityError> {
        match selection {
            KnowledgeSpaceInspection::Assistant => Ok(assistant_space(authority)),
            KnowledgeSpaceInspection::Agent { agent_id } => {
                if agent_id.is_empty()
                    || !agent_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte))
                {
                    return Err(invalid(
                        &json!(agent_id),
                        "an Agent identity with letters, digits, dash, underscore or colon",
                    ));
                }
                Ok(agent::agent_space(authority, &agent_id))
            }
            KnowledgeSpaceInspection::McpProject { project_root } => {
                let project = resolve_project(semantic, &project_root, authority, control)?;
                Ok(mcp::mcp_space(authority, &project))
            }
        }
    }

    pub(super) fn current_inspection(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        let frame = inspection_frame(context).ok_or_else(|| unbound(context))?;
        let binding = self
            .inspection_bindings
            .lock()
            .map_err(|_| unbound(context))?
            .get(&frame)
            .filter(|binding| binding.deadline > Instant::now())
            .cloned()
            .ok_or_else(|| unbound(context))?;
        let authority = self.identity(control)?;
        let mut grants = project_grants(semantic, &authority, control)?;
        if let Some(project) = &binding.space.shared_project {
            let grant = resolve_grant(semantic, &project.project_root, &authority, control)?;
            if grant.project != *project {
                return Err(invalid(
                    &json!(project),
                    "the original inspected project lineage and authority",
                ));
            }
            grants.retain(|existing| existing.project.project_root != project.project_root);
            grants.push(grant);
        }
        Ok(KnowledgeAccessContext {
            principal_kind: KnowledgePrincipalKind::Human,
            principal_id: "local-human".into(),
            security_domain: authority.clone(),
            storage_authority: authority,
            project_grants: grants,
            active_project: binding.space.shared_project.clone(),
            source_projects: Vec::new(),
            space: Some(binding.space),
        })
    }
}
