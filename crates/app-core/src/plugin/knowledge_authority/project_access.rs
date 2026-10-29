//! Converts fresh adapter membership into host grants. Catalog roots only discover candidates;
//! neither cached names nor public graph visibility establish private memory authority.
use super::*;
use lumvise_db_core::SemanticProjectAccess;

impl KnowledgeAuthority {
    pub(super) fn project_access(
        &self,
        plugin: &str,
        root: &str,
        semantic: &dyn SemanticPersistence,
        context: &PluginInvocationContext,
        control: &InvocationControl,
    ) -> Result<KnowledgeProjectGrant, HostCapabilityError> {
        if plugin != ASSISTANT {
            return Err(unbound(context));
        }
        if native_inbox_origin(context) {
            return resolve_grant(semantic, root, &self.identity(control)?, control);
        }
        self.current(semantic, context, control)?
            .project_grants
            .into_iter()
            .find(|grant| grant.project.project_root == root)
            .ok_or_else(|| invalid(&json!(root), "a currently granted inbox project"))
    }
}

fn native_inbox_origin(context: &PluginInvocationContext) -> bool {
    context.is_human_interaction()
        && context.scope_id().is_none()
        && agent::agent_frame(context).is_none()
        && context.plugin_chain().last().is_some_and(|frame| {
            frame.plugin_id == ASSISTANT
                && matches!(
                    frame.export_id.as_str(),
                    "send_assistant_message"
                        | "list_assistant_inbox"
                        | "mark_assistant_message_read"
                )
        })
}

pub(super) fn project_grants(
    semantic: &dyn SemanticPersistence,
    authority: &str,
    control: &InvocationControl,
) -> Result<Vec<KnowledgeProjectGrant>, HostCapabilityError> {
    let result = semantic
        .execute(SemanticOperation::ProjectRoots, control)
        .map_err(scope_failure)?;
    let SemanticResult::ProjectRoots(roots) = result else {
        return Err(invalid(&json!(result), "project roots"));
    };
    let mut grants = Vec::new();
    for root in roots.iter().filter(|root| !is_memory_root(root)) {
        if let Some(grant) = discover_project_grant(semantic, root, authority, control)? {
            grants.push(grant);
        }
    }
    Ok(grants)
}

fn discover_project_grant(
    semantic: &dyn SemanticPersistence,
    root: &str,
    authority: &str,
    control: &InvocationControl,
) -> Result<Option<KnowledgeProjectGrant>, HostCapabilityError> {
    match read_grant(semantic, root, authority, control) {
        Ok(grant) => Ok(grant),
        Err(error) if control.is_cancelled() || control.is_expired() => Err(error),
        Err(error) => {
            // An unavailable catalog entry grants no access; bound source scopes still fail closed.
            tracing::debug!(project_root = root, error = %error, "memory project membership unavailable");
            Ok(None)
        }
    }
}

pub(super) fn resolve_project(
    semantic: &dyn SemanticPersistence,
    root: &str,
    authority: &str,
    control: &InvocationControl,
) -> Result<KnowledgeProjectIdentity, HostCapabilityError> {
    resolve_grant(semantic, root, authority, control).map(|grant| grant.project)
}

pub(super) fn resolve_grant(
    semantic: &dyn SemanticPersistence,
    root: &str,
    authority: &str,
    control: &InvocationControl,
) -> Result<KnowledgeProjectGrant, HostCapabilityError> {
    read_grant(semantic, root, authority, control)?
        .ok_or_else(|| invalid(&json!(root), "current project membership"))
}

fn read_grant(
    semantic: &dyn SemanticPersistence,
    root: &str,
    authority: &str,
    control: &InvocationControl,
) -> Result<Option<KnowledgeProjectGrant>, HostCapabilityError> {
    if is_memory_root(root) || root.trim().is_empty() {
        return Err(invalid(&json!(root), "ordinary authorized project root"));
    }
    let result = semantic
        .execute(
            SemanticOperation::ProjectAccess {
                project_root: root.into(),
            },
            control,
        )
        .map_err(scope_failure)?;
    let SemanticResult::ProjectAccess(access) = result else {
        return Err(invalid(&json!(result), "fresh project access facts"));
    };
    grant_from_access(root, authority, access)
}

fn grant_from_access(
    root: &str,
    authority: &str,
    access: SemanticProjectAccess,
) -> Result<Option<KnowledgeProjectGrant>, HostCapabilityError> {
    let (project_id, storage_authority, writable) = match access {
        SemanticProjectAccess::Local { project_id } => (project_id, authority.to_owned(), true),
        SemanticProjectAccess::Member {
            project_id,
            storage_authority,
            writable,
        } => (project_id, storage_authority, writable),
        SemanticProjectAccess::Denied => return Ok(None),
    };
    if project_id.trim().is_empty() || storage_authority.trim().is_empty() {
        return Err(invalid(
            &json!([project_id, storage_authority]),
            "nonempty lineage and storage authority",
        ));
    }
    Ok(Some(KnowledgeProjectGrant {
        project: KnowledgeProjectIdentity {
            project_id,
            project_root: root.into(),
            storage_authority,
        },
        writable,
    }))
}

pub(super) fn memory_writable(scope: &KnowledgeAccessContext) -> bool {
    scope
        .space
        .as_ref()
        .and_then(|space| space.shared_project.as_ref())
        .is_none_or(|project| {
            scope
                .project_grants
                .iter()
                .any(|grant| grant.project == *project && grant.writable)
        })
}
