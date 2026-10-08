use lumvise_mcp_core::McpTool;
use serde_json::{Value, json};

pub(crate) fn tool_catalog(bridge_tools: &[Value]) -> Result<Vec<McpTool>, serde_json::Error> {
    let mut tools = Vec::with_capacity(bridge_tools.len() + 4);
    tools.extend([
        discover_app_plugins_tool(),
        invoke_app_plugin_capability_tool(),
        app_bridge_status_tool(),
        set_current_project_tool(),
    ]);
    tools.extend(bridge_tools.iter().cloned());
    tools.into_iter().map(McpTool::from_value).collect()
}

fn set_current_project_tool() -> Value {
    json!({
        "name": "set_current_project",
        "description": "Explicitly bind this initialized MCP connection to your current project, or pass null to unbind. Call after startup/resume and when changing projects. This only controls live desktop project presence; it does not import projects or change execution-provider routing, settings or working directory. Success returns connection_id, project_root and binding_status (ready or unbound).",
        "inputSchema": {"type":"object", "required":["project_root"], "additionalProperties":false,
            "properties":{"project_root":{"type":["string","null"],"minLength":1,"description":"Exact existing absolute project directory, or null to unbind."}}}
    })
}

fn discover_app_plugins_tool() -> Value {
    json!({
        "name": "discover_app_plugins",
        "description": "Discover the currently linked Lumvise app's installed plugin capabilities and their plugin-owned workflow guidance. Call at startup/resume and after plugin changes; use each capability's description and input schema. Check freshness and availability=ready: stopped plugins may remain listed as unavailable, and a stale inventory does not prove a tool is ready. Missing plugins are not available; session-scoped tools require their active scope.",
        "inputSchema": object_schema(Vec::<&'static str>::new())
    })
}

fn invoke_app_plugin_capability_tool() -> Value {
    json!({
        "name": "invoke_app_plugin_capability",
        "description": "Invoke a capability on the currently linked Lumvise app plugin surface.",
        "inputSchema": object_schema(["plugin_id", "capability_id", "input"])
    })
}

fn app_bridge_status_tool() -> Value {
    json!({
        "name": "app_bridge_status",
        "description": "Report the current MCP-to-app bridge link state.",
        "inputSchema": object_schema(Vec::<&'static str>::new())
    })
}

fn object_schema(required: impl IntoIterator<Item = &'static str>) -> Value {
    json!({ "type": "object", "required": required.into_iter().collect::<Vec<_>>() })
}
