//! A project write grant alone cannot promote a delegated Agent into a repair operator.
use super::*;

impl KnowledgeSemanticAccess<'_> {
    pub(super) fn authorize_recovery(
        &self,
        root: &str,
        scope: Option<&KnowledgeAccessContext>,
    ) -> lumvise_db_core::Result<()> {
        require_recovery_entry(self.context, scope)?;
        self.require_source(root, scope)?;
        self.require_root(root, scope, true)
    }
}

fn require_recovery_entry(
    context: &PluginInvocationContext,
    scope: Option<&KnowledgeAccessContext>,
) -> lumvise_db_core::Result<()> {
    let direct_export = matches!(context.plugin_chain(), [frame] if frame.plugin_id == "builtin.semantic"
        && matches!(frame.export_id.as_str(), "inspect_revision_integrity" | "recover_revision_integrity"));
    let human = context.is_human_interaction() && context.scope_id().is_none();
    let mounted_owner = context.scope_id() == Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE)
        && context.scope_epoch().is_some()
        && scope.is_some_and(|scope| scope.principal_kind == KnowledgePrincipalKind::Mcp);
    if direct_export && (human || mounted_owner) {
        return Ok(());
    }
    Err(DbError::invalid_value(
        format!(
            "scope={:?}, chain={:?}",
            context.scope_id(),
            context.plugin_chain()
        ),
        "a direct host-validated native owner or currently bound owner MCP repair call; no delegated or Agent invocation",
    ))
}
