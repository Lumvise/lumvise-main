//! App-owned publication calls outlive a status request, never its borrowed runtime channel.
use super::*;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    HostCapabilityError, HostPluginInvocation, HostPluginInvoker, PluginInvocationClass,
    PluginInvocationContext, PluginSystem,
};
use std::sync::Weak;

pub(crate) trait ImportAuthorization: Send + Sync {
    fn check(&self, control: &InvocationControl) -> Result<(), HostCapabilityError>;
}

pub(crate) trait ImportPublicationPort: HostPluginInvoker {
    fn authorize(&self, _control: &InvocationControl) -> Result<(), HostCapabilityError> {
        Ok(())
    }
}

pub(crate) struct ProjectImportPublisher {
    runtime: Weak<PluginSystem>,
    scope: Option<PluginInvocationContext>,
    authorization: Option<Arc<dyn ImportAuthorization>>,
}

impl ProjectImportPublisher {
    pub(crate) fn new(
        runtime: Weak<PluginSystem>,
        scope: Option<PluginInvocationContext>,
        authorization: Option<Arc<dyn ImportAuthorization>>,
    ) -> Self {
        Self {
            runtime,
            scope,
            authorization,
        }
    }

    fn invocation(&self) -> PluginInvocationContext {
        let context = PluginInvocationContext::new(
            format!("project-index-{}", uuid::Uuid::new_v4()),
            self.scope
                .as_ref()
                .map_or(IMPORTER_PLUGIN_ID, |scope| scope.owner_id()),
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(60),
        );
        let Some(scope) = &self.scope else {
            return context;
        };
        context
            .with_route_identity(
                scope.session_id().unwrap_or_default(),
                scope.scope_id().map(str::to_owned),
            )
            .with_scope_epoch(scope.scope_epoch())
    }
}

impl ImportPublicationPort for ProjectImportPublisher {
    fn authorize(&self, control: &InvocationControl) -> Result<(), HostCapabilityError> {
        if let Some(authorization) = &self.authorization {
            authorization.check(control)?;
        }
        Ok(())
    }
}

impl HostPluginInvoker for ProjectImportPublisher {
    fn invoke_plugin(&self, request: HostPluginInvocation) -> Result<Value, HostCapabilityError> {
        self.authorize(&InvocationControl::sixty_seconds())?;
        if request.plugin_id != SEMANTIC_PLUGIN_ID {
            return Err(publication_error(format!(
                "target {}; expected builtin.semantic",
                request.plugin_id
            )));
        }
        let export = request.export_id.clone();
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| publication_error("plugin runtime stopped"))?;
        let outcome = runtime
            .invoke_controlled(lumvise_plugin_runtime::PluginInvocationRequest::new(
                request.plugin_id,
                request.export_id,
                request.input,
                self.invocation(),
            ))
            .map_err(|error| {
                publication_error(format!("builtin.semantic/{export} unavailable: {error}"))
            })?;
        match outcome {
            WireOutcome::Succeeded { value } => Ok(value),
            WireOutcome::Failed { error } => Err(HostCapabilityError::new(
                "project.import",
                error.code,
                error.message,
                error.retryable,
            )),
        }
    }
}

fn publication_error(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new("project.import", "semantic_unavailable", message, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_owned_publication_preserves_authority_without_a_semantic_call_cycle() {
        let source = PluginInvocationContext::new(
            "expired-request",
            "mcp-owner",
            PluginInvocationClass::Foreground,
            Instant::now() - Duration::from_secs(1),
        )
        .with_route_identity(
            "mcp-connection",
            Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE.into()),
        )
        .with_scope_epoch(Some(7))
        .enter_plugin("builtin.semantic", "index_project");
        let publisher = ProjectImportPublisher::new(Weak::new(), Some(source), None);
        let invocation = publisher.invocation();
        assert_eq!(invocation.owner_id(), "mcp-owner");
        assert_eq!(invocation.session_id(), Some("mcp-connection"));
        assert_eq!(invocation.scope_epoch(), Some(7));
        assert!(invocation.plugin_chain().is_empty());
        assert!(
            (59..=60).contains(
                &invocation
                    .deadline()
                    .saturating_duration_since(Instant::now())
                    .as_secs()
            )
        );
    }
}
