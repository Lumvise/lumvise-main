use crate::{AppCoreError, PluginEndpoints, PluginInvocationRequest, PluginInvocationStatus};
use lumvise_plugin_runtime::{PluginInvocationCancellationRequest, PluginInvocationFailureKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use super::PluginInvocationLifecycle;

pub const PLUGIN_MCP_SSE_ENDPOINT: &str = "/api/plugin-mcp/sse";
pub const PLUGIN_MCP_MESSAGE_ENDPOINT: &str = "/api/plugin-mcp/messages";

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

pub(crate) fn handle_plugin_mcp_json_rpc(
    plugins: &PluginEndpoints<'_>,
    request: Value,
) -> crate::Result<Option<Value>> {
    let request: JsonRpcRequest = decode_rpc_request(request)?;
    if request.id.is_none() {
        handle_notification(plugins, &request.method, request.params)?;
        return Ok(None);
    }
    let id = request
        .id
        .ok_or_else(|| AppCoreError::missing_value("id", "plugin MCP request id"))?;
    let result = dispatch_plugin_mcp_method(plugins, &id, &request.method, request.params);
    encode_rpc_response(response_from_result(Some(id), result))
}

fn dispatch_plugin_mcp_method(
    plugins: &PluginEndpoints<'_>,
    request_id: &Value,
    method: &str,
    params: Option<Value>,
) -> std::result::Result<Value, JsonRpcError> {
    match method {
        "initialize" => Ok(initialize_result()),
        "tools/list" => tools_list(plugins),
        "tools/call" => call_tool(plugins, request_id, params),
        other => Err(method_not_found(other)),
    }
}

fn tools_list(plugins: &PluginEndpoints<'_>) -> std::result::Result<Value, JsonRpcError> {
    let tools = plugins
        .plugin_mcp_tools()
        .map_err(tool_error)?
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.tool_name,
                "description": tool.description,
                "inputSchema": tool.mcp_schema()
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "tools": tools }))
}

fn call_tool(
    plugins: &PluginEndpoints<'_>,
    request_id: &Value,
    params: Option<Value>,
) -> std::result::Result<Value, JsonRpcError> {
    let call = parse_tool_call(params)?;
    let response = plugins
        .invoke_plugin_mcp_tool_controlled(
            PluginInvocationRequest {
                tool_name: call.name,
                arguments: call.arguments,
            },
            global_lifecycle(request_id),
        )
        .map_err(tool_error)?;
    tool_result(
        response.output,
        response.status == PluginInvocationStatus::Failed,
    )
}

fn global_lifecycle(request_id: &Value) -> PluginInvocationLifecycle {
    PluginInvocationLifecycle {
        request_id: request_key(request_id),
        owner_id: "plugin-mcp-http".into(),
        session_id: "plugin-mcp-http".into(),
        scope_id: None,
        deadline: Instant::now() + Duration::from_secs(60),
    }
}

fn handle_notification(
    plugins: &PluginEndpoints<'_>,
    method: &str,
    params: Option<Value>,
) -> crate::Result<()> {
    if method != "notifications/cancelled" {
        return Ok(());
    }
    let request_id = params
        .and_then(|params| params.get("requestId").cloned())
        .ok_or_else(|| AppCoreError::missing_value("requestId", "MCP cancellation identity"))?;
    let _ = plugins
        .app
        .plugin_system()
        .cancel_controlled(&PluginInvocationCancellationRequest {
            plugin_id: None,
            request_id: request_key(&request_id),
            owner_id: "plugin-mcp-http".into(),
            session_id: Some("plugin-mcp-http".into()),
            scope_id: None,
        });
    Ok(())
}

fn request_key(request_id: &Value) -> String {
    serde_json::to_string(request_id).unwrap_or_else(|_| request_id.to_string())
}

fn parse_tool_call(params: Option<Value>) -> std::result::Result<ToolCallParams, JsonRpcError> {
    let params = params.ok_or_else(|| invalid_params("missing tools/call params"))?;
    serde_json::from_value(params).map_err(|error| invalid_params(error.to_string()))
}

fn tool_result(payload: Value, is_error: bool) -> std::result::Result<Value, JsonRpcError> {
    let text =
        serde_json::to_string(&payload).map_err(|error| invalid_params(error.to_string()))?;
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": { "name": "lumvise-plugin-mcp", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "tools": {} }
    })
}

fn decode_rpc_request(request: Value) -> crate::Result<JsonRpcRequest> {
    serde_json::from_value(request).map_err(|error| {
        AppCoreError::invalid_value(error.to_string(), "plugin MCP JSON-RPC request")
    })
}

fn encode_rpc_response(response: JsonRpcResponse) -> crate::Result<Option<Value>> {
    serde_json::to_value(response).map(Some).map_err(|error| {
        AppCoreError::invalid_value(error.to_string(), "plugin MCP JSON-RPC response")
    })
}

fn response_from_result(
    id: Option<Value>,
    result: std::result::Result<Value, JsonRpcError>,
) -> JsonRpcResponse {
    match result {
        Ok(value) => JsonRpcResponse::success(id, value),
        Err(error) => JsonRpcResponse::failure(id, error),
    }
}

fn method_not_found(method: &str) -> JsonRpcError {
    JsonRpcError {
        code: -32601,
        message: format!("method {method:?} was not found"),
        data: None,
    }
}

fn invalid_params(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: -32602,
        message: message.into(),
        data: None,
    }
}

fn tool_error(error: AppCoreError) -> JsonRpcError {
    let AppCoreError::ControlledPluginRuntime(runtime) = error else {
        return JsonRpcError {
            code: -32000,
            message: error.to_string(),
            data: None,
        };
    };
    let kind = runtime.kind();
    JsonRpcError {
        code: runtime_error_code(kind),
        message: runtime.to_string(),
        data: Some(json!({ "kind": runtime_error_name(kind), "retryable": runtime.retryable() })),
    }
}

fn runtime_error_code(kind: PluginInvocationFailureKind) -> i64 {
    match kind {
        PluginInvocationFailureKind::Busy => -32001,
        PluginInvocationFailureKind::DeadlineExceeded => -32002,
        PluginInvocationFailureKind::Cancelled => -32003,
        PluginInvocationFailureKind::Unavailable => -32004,
        _ => -32000,
    }
}

fn runtime_error_name(kind: PluginInvocationFailureKind) -> &'static str {
    match kind {
        PluginInvocationFailureKind::Busy => "busy",
        PluginInvocationFailureKind::DeadlineExceeded => "deadline_exceeded",
        PluginInvocationFailureKind::Cancelled => "cancelled",
        PluginInvocationFailureKind::Unavailable => "unavailable",
        PluginInvocationFailureKind::InvalidInput => "invalid_input",
        PluginInvocationFailureKind::PluginFailure => "plugin_failure",
        PluginInvocationFailureKind::Internal => "internal",
    }
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn failure(id: Option<Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}
