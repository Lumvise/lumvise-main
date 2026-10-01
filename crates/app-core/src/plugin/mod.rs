//! Compiled-plugin AppCore boundary.

mod background_delivery;
mod change_hook_delivery;
mod compiled_sse;
pub(crate) mod compiled_surfaces;
pub(crate) mod compiled_views;
mod host_capability_broker;
mod host_capability_catalog;
pub(crate) mod http_dispatch;
mod invocation;
mod mcp_catalog;
pub(crate) mod mcp_http_bridge;
pub(crate) mod mcp_rpc;
mod mcp_types;
mod plugin_host_capabilities;
mod plugin_invoke_authorization;
pub(crate) mod production;
mod scoped_mcp;
pub(crate) mod scoped_mcp_channel;
pub(crate) mod scoped_mcp_rpc;
mod semantic_snapshot_writes;
mod semantic_storage_capability;
mod tasks;
mod voice_playback_transport;

use std::sync::Arc;

use lumvise_db_core::ArtifactTextVectorizer;
use serde_json::Value;

use crate::{AppCore, AppCoreError};

use mcp_catalog::{McpCatalog, McpToolRoute};

pub use change_hook_delivery::{ChangeHookCycleReport, ChangeHookRun};
pub use host_capability_broker::AppCoreHostCapabilityBroker;
pub(crate) use host_capability_broker::{SharedPluginVectorizer, VectorEngineIdentity};
pub use host_capability_catalog::compiled_host_capability_versions;
pub(crate) use invocation::PluginInvocationLifecycle;
#[cfg(feature = "desktop-app")]
pub(crate) use invocation::surface_invocation_context;
pub use invocation::{PluginInvocationRequest, PluginInvocationResponse, PluginInvocationStatus};
pub use mcp_rpc::{PLUGIN_MCP_MESSAGE_ENDPOINT, PLUGIN_MCP_SSE_ENDPOINT};
pub use mcp_types::PluginMcpTool;
pub(crate) use plugin_host_capabilities::PluginHostServices;
pub use production::{LUMVISE_PLUGIN_ROOT_ENV, PluginProductionConfig};
pub use scoped_mcp_channel::{
    SCOPED_MCP_MESSAGE_ENDPOINT, SCOPED_MCP_SSE_ENDPOINT, ScopedMcpChannel,
    ScopedMcpMessageRequest, ScopedMcpToolRoute,
};
pub use tasks::{PluginRecurringTask, PluginRecurringTaskRun};
#[cfg(feature = "desktop-app")]
pub(crate) use voice_playback_transport::VoicePlaybackTransportEvent;

/// Typed entrypoint for AppCore services and cataloged compiled plugin exports.
pub struct PluginEndpoints<'app> {
    pub(crate) app: &'app AppCore,
}

impl<'app> PluginEndpoints<'app> {
    pub(crate) fn new(app: &'app AppCore) -> Self {
        Self { app }
    }

    /// Lists MCP tools from ready compiled plugins.
    pub fn plugin_mcp_tools(&self) -> crate::Result<Vec<PluginMcpTool>> {
        Ok(McpCatalog::load(self)?.tools())
    }

    /// Lists ready compiled View exports with signed renderer constraints.
    pub fn compiled_plugin_views(
        &self,
    ) -> crate::Result<Vec<lumvise_frontend_core::RendererViewDescriptor>> {
        Ok(compiled_surfaces::CompiledSurfaceCatalog::load(self.app)?
            .views()
            .to_vec())
    }

    /// Invokes one ready compiled MCP export when its process is ready.
    pub fn invoke_plugin_mcp_tool(
        &self,
        request: PluginInvocationRequest,
    ) -> crate::Result<PluginInvocationResponse> {
        let route = McpCatalog::load(self)?.route(&request.tool_name)?;
        self.invoke_plugin_mcp_tool_inner(request, route)
    }

    pub(crate) fn invoke_plugin_mcp_tool_controlled(
        &self,
        request: PluginInvocationRequest,
        lifecycle: PluginInvocationLifecycle,
    ) -> crate::Result<PluginInvocationResponse> {
        let route = McpCatalog::load(self)?.route(&request.tool_name)?;
        mcp_catalog::invoke_compiled_controlled(
            self,
            &route.plugin_id,
            &route.export_id,
            request.arguments,
            lifecycle,
        )
    }

    /// Builds the scoped MCP channel for one signed scope and session.
    pub fn scoped_mcp_channel(
        &self,
        scope_id: &str,
        session_id: &str,
    ) -> crate::Result<ScopedMcpChannel> {
        scoped_mcp_channel::scoped_mcp_channel(
            scope_id,
            session_id,
            self.scoped_mcp_tools(scope_id)?,
        )
    }

    /// Invokes one scoped MCP message through the owning Plugin route.
    pub fn invoke_scoped_mcp_message(
        &self,
        request: ScopedMcpMessageRequest,
    ) -> crate::Result<PluginInvocationResponse> {
        let scope_id = request.scope_id.clone();
        let invocation = scoped_mcp_channel::plugin_invocation_from_message(request)?;
        self.invoke_scoped_mcp_tool(&scope_id, invocation)
    }

    /// Handles one app Plugin MCP JSON-RPC request.
    pub fn handle_plugin_mcp_json_rpc(&self, request: Value) -> crate::Result<Option<Value>> {
        mcp_rpc::handle_plugin_mcp_json_rpc(self, request)
    }

    pub(crate) fn handle_scoped_mcp_json_rpc(
        &self,
        request: Value,
        context: scoped_mcp_rpc::ScopedMcpRouteContext,
    ) -> crate::Result<Option<Value>> {
        scoped_mcp_rpc::handle_scoped_mcp_json_rpc_for_context(self, request, context)
    }

    fn invoke_plugin_mcp_tool_inner(
        &self,
        request: PluginInvocationRequest,
        route: McpToolRoute,
    ) -> crate::Result<PluginInvocationResponse> {
        mcp_catalog::invoke_compiled(self, &route.plugin_id, &route.export_id, request)
    }

    /// Installs a host embedding provider for permission-gated `neural.embed` calls.
    ///
    /// Probes the vectorizer once to learn its own `engine_id`/`model` identity
    /// (the same values it will report on every subsequent embed) so background
    /// vector maintenance can tell which stored vectors match the live engine
    /// without guessing at a separately-declared label.
    ///
    /// # Example
    /// ```ignore
    /// app.plugin_endpoints().set_plugin_vectorizer(vectorizer)?;
    /// ```
    pub fn set_plugin_vectorizer(
        &self,
        vectorizer: Box<dyn ArtifactTextVectorizer + Send + Sync>,
    ) -> crate::Result<()> {
        let probe = vectorizer
            .vectorize_artifact_text(VECTOR_ENGINE_IDENTITY_PROBE_TEXT)
            .map_err(|error| {
                AppCoreError::invalid_value(
                    error.to_string(),
                    "vectorizer able to embed its identity probe text",
                )
            })?;
        let identity = VectorEngineIdentity {
            engine_id: probe.engine_id,
            model: probe.model,
        };
        let vectorizer: Arc<dyn ArtifactTextVectorizer + Send + Sync> = Arc::from(vectorizer);
        {
            let mut current = self
                .app
                .plugin_vectorizer
                .lock()
                .map_err(|_| AppCoreError::poisoned_mutex("plugin_vectorizer"))?;
            *current = Some(vectorizer);
        }
        let mut active = self
            .app
            .active_vector_engine
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("active_vector_engine"))?;
        *active = Some(identity);
        Ok(())
    }

    /// Removes the installed embedding provider; `neural.embed` reports
    /// `provider_unavailable` and search falls back to lexical matching.
    pub fn clear_plugin_vectorizer(&self) -> crate::Result<()> {
        {
            let mut current = self
                .app
                .plugin_vectorizer
                .lock()
                .map_err(|_| AppCoreError::poisoned_mutex("plugin_vectorizer"))?;
            *current = None;
        }
        let mut active = self
            .app
            .active_vector_engine
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("active_vector_engine"))?;
        *active = None;
        Ok(())
    }

    /// Returns the live vectorizer instance, if one is installed.
    pub(crate) fn plugin_vectorizer(
        &self,
    ) -> crate::Result<Option<Arc<dyn ArtifactTextVectorizer + Send + Sync>>> {
        Ok(self
            .app
            .plugin_vectorizer
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("plugin_vectorizer"))?
            .clone())
    }
}

const VECTOR_ENGINE_IDENTITY_PROBE_TEXT: &str = "lumvise-vector-engine-identity-probe";
