//! Generic MCP catalog backed only by ready exports for one signed scope.

use std::collections::BTreeMap;

use lumvise_plugin_runtime::PublishedPlugin;
use serde_json::Value;

use super::{
    PluginEndpoints, PluginInvocationLifecycle, PluginInvocationRequest, PluginInvocationResponse,
    PluginMcpTool,
};
use crate::AppCoreError;

#[derive(Clone)]
struct ScopedToolRoute {
    plugin_id: String,
    export_id: String,
}

struct ScopedMcpCatalog {
    tools: Vec<PluginMcpTool>,
    routes: BTreeMap<(String, String), ScopedToolRoute>,
}

impl PluginEndpoints<'_> {
    /// Lists ready compiled tools belonging to one signed scope.
    pub fn scoped_mcp_tools(&self, scope_id: &str) -> crate::Result<Vec<PluginMcpTool>> {
        Ok(ScopedMcpCatalog::load(self, scope_id)?.tools)
    }

    /// Invokes one ready compiled export belonging to one signed scope.
    pub fn invoke_scoped_mcp_tool(
        &self,
        scope_id: &str,
        request: PluginInvocationRequest,
    ) -> crate::Result<PluginInvocationResponse> {
        self.invoke_scoped_mcp_tool_inner(scope_id, request)
    }

    fn invoke_scoped_mcp_tool_inner(
        &self,
        scope_id: &str,
        request: PluginInvocationRequest,
    ) -> crate::Result<PluginInvocationResponse> {
        let plugin_id = scoped_mcp_plugin_id(&request.arguments)?;
        let route =
            ScopedMcpCatalog::load(self, scope_id)?.route(&plugin_id, &request.tool_name)?;
        invoke_compiled(self, &route.plugin_id, &route.export_id, request.arguments)
    }

    pub(crate) fn invoke_scoped_mcp_tool_controlled(
        &self,
        scope_id: &str,
        request: PluginInvocationRequest,
        lifecycle: PluginInvocationLifecycle,
    ) -> crate::Result<PluginInvocationResponse> {
        let plugin_id = scoped_mcp_plugin_id(&request.arguments)?;
        let route =
            ScopedMcpCatalog::load(self, scope_id)?.route(&plugin_id, &request.tool_name)?;
        super::mcp_catalog::invoke_compiled_controlled(
            self,
            &route.plugin_id,
            &route.export_id,
            request.arguments,
            lifecycle,
        )
    }
}

pub(crate) fn scoped_mcp_route(
    endpoints: &PluginEndpoints<'_>,
    scope_id: &str,
    plugin_id: &str,
    tool_name: &str,
) -> crate::Result<(String, String)> {
    let route = ScopedMcpCatalog::load(endpoints, scope_id)?.route(plugin_id, tool_name)?;
    Ok((route.plugin_id, route.export_id))
}

impl ScopedMcpCatalog {
    fn load(endpoints: &PluginEndpoints<'_>, scope_id: &str) -> crate::Result<Self> {
        let published = endpoints.app.plugin_system().scoped_exports(scope_id)?;
        build_catalog(compiled_candidates(published))
    }

    fn route(&self, plugin_id: &str, tool_name: &str) -> crate::Result<ScopedToolRoute> {
        self.routes
            .get(&(plugin_id.to_owned(), tool_name.to_owned()))
            .cloned()
            .ok_or_else(|| AppCoreError::invalid_value(tool_name, "ready scoped MCP tool"))
    }
}

fn compiled_candidates(plugins: Vec<PublishedPlugin>) -> Vec<(PluginMcpTool, ScopedToolRoute)> {
    plugins
        .into_iter()
        .flat_map(|plugin| {
            plugin.exports.into_iter().map(move |export| {
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
        })
        .collect()
}

fn candidate(
    plugin_id: &str,
    export_id: String,
    description: String,
    input_schema: Value,
) -> (PluginMcpTool, ScopedToolRoute) {
    let tool = PluginMcpTool {
        plugin_id: plugin_id.into(),
        tool_name: export_id.clone(),
        description,
        input_schema,
    };
    let route = ScopedToolRoute {
        plugin_id: plugin_id.into(),
        export_id,
    };
    (tool, route)
}

fn build_catalog(
    mut candidates: Vec<(PluginMcpTool, ScopedToolRoute)>,
) -> crate::Result<ScopedMcpCatalog> {
    candidates.sort_by(|left, right| {
        (&left.0.plugin_id, &left.0.tool_name).cmp(&(&right.0.plugin_id, &right.0.tool_name))
    });
    let mut tools = Vec::with_capacity(candidates.len());
    let mut routes = BTreeMap::new();
    for (tool, route) in candidates {
        let key = (tool.plugin_id.clone(), tool.tool_name.clone());
        if routes.insert(key.clone(), route).is_some() {
            return Err(AppCoreError::invalid_value(
                format!("{}:{}", key.0, key.1),
                "unique scoped MCP owner and tool name",
            ));
        }
        tools.push(tool);
    }
    Ok(ScopedMcpCatalog { tools, routes })
}

fn invoke_compiled(
    endpoints: &PluginEndpoints<'_>,
    plugin_id: &str,
    export_id: &str,
    input: Value,
) -> crate::Result<PluginInvocationResponse> {
    let lifecycle =
        super::invocation::local_invocation_lifecycle("app-core-scoped-mcp", Some(plugin_id));
    super::mcp_catalog::invoke_compiled_controlled(
        endpoints, plugin_id, export_id, input, lifecycle,
    )
}

fn scoped_mcp_plugin_id(arguments: &Value) -> crate::Result<String> {
    arguments
        .get("plugin_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppCoreError::missing_value("plugin_id", "scoped MCP plugin id"))
}
