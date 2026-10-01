use lumvise_mcp_core::McpTool;
use serde_json::{Value, json};

pub(crate) fn tool_catalog(bridge_tools: &[Value]) -> Result<Vec<McpTool>, serde_json::Error> {
    let mut tools = Vec::with_capacity(bridge_tools.len() + 3);
    tools.extend([
        discover_app_plugins_tool(),
        invoke_app_plugin_capability_tool(),
        app_bridge_status_tool(),
    ]);
    tools.extend(bridge_tools.iter().cloned());
    tools.into_iter().map(McpTool::from_value).collect()
}

fn discover_app_plugins_tool() -> Value {
    json!({
        "name": "discover_app_plugins",
        "description": "Discover plugin capabilities from the currently linked Lumvise app.",
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
