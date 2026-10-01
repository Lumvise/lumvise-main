use crate::PluginMcpTool;
use crate::{AppCoreError, PluginInvocationRequest, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SCOPED_MCP_SSE_ENDPOINT: &str = "/api/scoped-plugin-mcp/sse";
pub const SCOPED_MCP_MESSAGE_ENDPOINT: &str = "/api/scoped-plugin-mcp/messages";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedMcpChannel {
    pub scope_id: String,
    pub session_id: String,
    pub transport: String,
    pub sse_endpoint: String,
    pub message_endpoint: String,
    pub tools: Vec<ScopedMcpToolRoute>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedMcpToolRoute {
    pub plugin_id: String,
    pub tool_name: String,
    pub wire_tool_name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopedMcpMessageRequest {
    pub scope_id: String,
    pub owner_id: String,
    pub plugin_id: String,
    pub session_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub arguments: Value,
}

pub(crate) fn scoped_mcp_channel(
    scope_id: &str,
    session_id: &str,
    tools: Vec<PluginMcpTool>,
) -> Result<ScopedMcpChannel> {
    let scope_id = require_non_empty(scope_id, "scoped MCP scope id")?;
    let session_id = require_non_empty(session_id, "scoped MCP session id")?;
    Ok(ScopedMcpChannel {
        scope_id,
        session_id,
        transport: "sse".to_string(),
        sse_endpoint: SCOPED_MCP_SSE_ENDPOINT.to_string(),
        message_endpoint: SCOPED_MCP_MESSAGE_ENDPOINT.to_string(),
        tools: tools.into_iter().map(ScopedMcpToolRoute::from).collect(),
    })
}

pub(crate) fn plugin_invocation_from_message(
    request: ScopedMcpMessageRequest,
) -> Result<PluginInvocationRequest> {
    require_non_empty(&request.scope_id, "scoped MCP scope id")?;
    let owner_id = require_non_empty(&request.owner_id, "scoped MCP owner id")?;
    let plugin_id = require_non_empty(&request.plugin_id, "scoped MCP plugin id")?;
    let session_id = require_non_empty(&request.session_id, "scoped MCP session id")?;
    let tool_name = require_non_empty(&request.tool_name, "scoped MCP tool name")?;
    Ok(PluginInvocationRequest {
        tool_name,
        arguments: message_arguments(request.arguments, owner_id, plugin_id, session_id)?,
    })
}

fn message_arguments(
    arguments: Value,
    owner_id: String,
    plugin_id: String,
    session_id: String,
) -> Result<Value> {
    let mut object = match arguments {
        Value::Null => Map::new(),
        Value::Object(object) => object,
        other => return Err(invalid_arguments_shape(other)),
    };
    object.insert("mcp_owner_id".to_string(), Value::String(owner_id));
    object.insert("plugin_id".to_string(), Value::String(plugin_id));
    object.insert("session_id".to_string(), Value::String(session_id));
    Ok(Value::Object(object))
}

fn invalid_arguments_shape(value: Value) -> AppCoreError {
    AppCoreError::invalid_value(value.to_string(), "scoped MCP message arguments object")
}

fn require_non_empty(value: &str, expected: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppCoreError::missing_value(value, expected));
    }
    Ok(value.to_string())
}

impl From<PluginMcpTool> for ScopedMcpToolRoute {
    fn from(tool: PluginMcpTool) -> Self {
        let wire_tool_name = scoped_mcp_wire_tool_name(&tool.plugin_id, &tool.tool_name);
        let input_schema = tool.mcp_schema();
        Self {
            plugin_id: tool.plugin_id,
            tool_name: tool.tool_name,
            wire_tool_name,
            description: tool.description,
            input_schema,
        }
    }
}

pub(crate) fn scoped_mcp_wire_tool_name(plugin_id: &str, tool_name: &str) -> String {
    let plugin = sanitized_tool_name_segment(plugin_id);
    let tool = sanitized_tool_name_segment(tool_name);
    format!("{plugin}__{tool}")
}

fn sanitized_tool_name_segment(value: &str) -> String {
    value
        .chars()
        .map(|item| match item {
            'a'..='z' | 'A'..='Z' | '0'..='9' => item,
            _ => '_',
        })
        .collect()
}
