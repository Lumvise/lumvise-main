//! Global MCP catalog derived from installed compiled exports.
//!
//! Uses `cataloged_plugins` (all installed), not `published_plugins` (ready only),
//! so a stopped or not-yet-started plugin keeps its MCP route and reports temporary
//! unavailability on invocation instead of vanishing from `tools/list`. This contract
//! is covered by `compiled_plugin_restart_keeps_route_and_recovers_invocation`.

use std::collections::BTreeMap;

use lumvise_plugin_package::ExportSurface;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    PluginInvocationClass, PluginInvocationContext,
    PluginInvocationRequest as RuntimeInvocationRequest, PublishedPlugin,
};
use serde_json::json;

use super::{
    PluginEndpoints, PluginInvocationLifecycle, PluginInvocationRequest, PluginInvocationResponse,
    PluginInvocationStatus, PluginMcpTool,
};
use crate::{AppCoreError, Result};

#[derive(Clone)]
pub(crate) struct McpToolRoute {
    pub(crate) plugin_id: String,
    pub(crate) export_id: String,
}

pub(crate) struct McpCatalog {
    tools: Vec<PluginMcpTool>,
    routes: BTreeMap<String, McpToolRoute>,
}

impl McpCatalog {
    pub(crate) fn load(endpoints: &PluginEndpoints<'_>) -> Result<Self> {
        let candidates = compiled_candidates(endpoints.app.plugin_system().cataloged_plugins()?);
        finalize_catalog(candidates)
    }

    pub(crate) fn tools(&self) -> Vec<PluginMcpTool> {
        self.tools.clone()
    }

    pub(crate) fn route(&self, tool_name: &str) -> Result<McpToolRoute> {
        self.routes
            .get(tool_name)
            .cloned()
            .ok_or_else(|| AppCoreError::invalid_value(tool_name, "ready compiled plugin tool"))
    }
}

pub(crate) fn invoke_compiled(
    endpoints: &PluginEndpoints<'_>,
    plugin_id: &str,
    export_id: &str,
    request: PluginInvocationRequest,
) -> Result<PluginInvocationResponse> {
    let lifecycle = super::invocation::local_invocation_lifecycle("app-core-mcp", None);
    invoke_compiled_controlled(
        endpoints,
        plugin_id,
        export_id,
        request.arguments,
        lifecycle,
    )
}

pub(crate) fn invoke_compiled_controlled(
    endpoints: &PluginEndpoints<'_>,
    plugin_id: &str,
    export_id: &str,
    arguments: serde_json::Value,
    lifecycle: PluginInvocationLifecycle,
) -> Result<PluginInvocationResponse> {
    let context = PluginInvocationContext::new(
        lifecycle.request_id,
        lifecycle.owner_id,
        PluginInvocationClass::Foreground,
        lifecycle.deadline,
    )
    .with_route_identity(lifecycle.session_id, lifecycle.scope_id);
    let request = RuntimeInvocationRequest::new(plugin_id, export_id, arguments, context);
    let outcome = endpoints.app.plugin_system().invoke_controlled(request)?;
    Ok(invocation_response(outcome))
}

fn compiled_candidates(mut plugins: Vec<PublishedPlugin>) -> Vec<CatalogCandidate> {
    plugins.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    plugins
        .into_iter()
        .flat_map(compiled_plugin_candidates)
        .collect()
}

fn compiled_plugin_candidates(plugin: PublishedPlugin) -> Vec<CatalogCandidate> {
    plugin
        .exports
        .into_iter()
        .filter(|export| matches!(export.surface, ExportSurface::McpTool))
        .map(|export| {
            let description = if export.description.trim().is_empty() {
                export.name
            } else {
                export.description
            };
            candidate(
                &plugin.plugin_id,
                export.id,
                description,
                export.input_schema,
            )
        })
        .collect()
}

fn candidate(
    plugin_id: &str,
    export_id: String,
    description: String,
    input_schema: serde_json::Value,
) -> CatalogCandidate {
    let tool_name = format!("app_plugin.{plugin_id}.{export_id}");
    CatalogCandidate {
        tool: PluginMcpTool {
            plugin_id: plugin_id.into(),
            tool_name,
            description,
            input_schema,
        },
        route: McpToolRoute {
            plugin_id: plugin_id.into(),
            export_id,
        },
    }
}

fn finalize_catalog(mut candidates: Vec<CatalogCandidate>) -> Result<McpCatalog> {
    candidates.sort_by(|left, right| left.tool.tool_name.cmp(&right.tool.tool_name));
    reject_collisions(&candidates)?;
    let mut tools = Vec::with_capacity(candidates.len());
    let mut routes = BTreeMap::new();
    for candidate in candidates {
        routes.insert(candidate.tool.tool_name.clone(), candidate.route);
        tools.push(candidate.tool);
    }
    Ok(McpCatalog { tools, routes })
}

fn reject_collisions(candidates: &[CatalogCandidate]) -> Result<()> {
    for collision in candidates.windows(2) {
        if collision[0].tool.tool_name != collision[1].tool.tool_name {
            continue;
        }
        return Err(AppCoreError::PluginMcpCollision {
            tool_name: collision[0].tool.tool_name.clone(),
            plugin_ids: vec![
                collision[0].tool.plugin_id.clone(),
                collision[1].tool.plugin_id.clone(),
            ],
        });
    }
    Ok(())
}

fn invocation_response(outcome: WireOutcome) -> PluginInvocationResponse {
    match outcome {
        WireOutcome::Succeeded { value } => PluginInvocationResponse {
            status: PluginInvocationStatus::Completed,
            output: value,
            job_id: None,
        },
        WireOutcome::Failed { error } => PluginInvocationResponse {
            status: PluginInvocationStatus::Failed,
            output: json!({"error": {"code": error.code, "message": error.message,
                "details": error.details, "retryable": error.retryable}}),
            job_id: None,
        },
    }
}

struct CatalogCandidate {
    tool: PluginMcpTool,
    route: McpToolRoute,
}
