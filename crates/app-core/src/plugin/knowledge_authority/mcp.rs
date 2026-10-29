//! Project sharing derives from the host registry, never a model-provided root.
use super::*;
use sha2::{Digest, Sha256};

impl KnowledgeAuthority {
    pub(super) fn current_mcp(
        &self,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeAccessContext, HostCapabilityError> {
        let connection = context.session_id().ok_or_else(|| unbound(context))?;
        let binding = self
            .mcp_membership
            .scope(connection, context.scope_epoch())
            .map_err(|error| invalid(&json!(connection), &error))?;
        let authority = self.identity(control)?;
        let grant = resolve_grant(semantic, &binding.project_root, &authority, control)?;
        let project = grant.project.clone();
        if project.project_id != binding.project_id
            || project.storage_authority
                != binding.storage_authority.as_deref().unwrap_or(&authority)
        {
            return Err(invalid(
                &json!(binding.project_root),
                "current authorized project with the captured lineage",
            ));
        }
        self.mcp_membership
            .scope(connection, context.scope_epoch())
            .map_err(|error| invalid(&json!(connection), &error))?;
        Ok(mcp_access(authority, project, grant))
    }
}

fn mcp_access(
    authority: String,
    project: KnowledgeProjectIdentity,
    grant: KnowledgeProjectGrant,
) -> KnowledgeAccessContext {
    KnowledgeAccessContext {
        principal_kind: KnowledgePrincipalKind::Mcp,
        principal_id: project.project_id.clone(),
        space: Some(mcp_space(&authority, &project)),
        security_domain: authority.clone(),
        storage_authority: authority,
        project_grants: vec![grant],
        active_project: Some(project.clone()),
        source_projects: vec![project],
    }
}

pub(super) fn mcp_space(
    authority: &str,
    project: &KnowledgeProjectIdentity,
) -> KnowledgeSpaceDescriptor {
    let domain = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&project.storage_authority, &project.project_id))
                .expect("project identity serializes")
        )
    );
    KnowledgeSpaceDescriptor {
        space_id: format!("mcp:{authority}:{domain}"),
        space_kind: KnowledgeSpaceKind::McpProject,
        project_root: format!("mcp-space://{authority}/{domain}"),
        principal_id: project.project_id.clone(),
        security_domain: authority.into(),
        storage_authority: authority.into(),
        shared_project: Some(project.clone()),
        revision: 1,
    }
}
