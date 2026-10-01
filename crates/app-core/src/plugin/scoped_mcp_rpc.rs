use crate::{AppCoreError, PluginEndpoints, ScopedMcpMessageRequest, ScopedMcpToolRoute};
use lumvise_neural_core::llm_providers::tool_invocation::{McpTool, McpToolCatalog, ToolOutcome};
use lumvise_plugin_runtime::{PluginInvocationCancellationRequest, PluginInvocationFailureKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use super::{PluginInvocationLifecycle, PluginInvocationStatus, scoped_mcp_channel};

const SCOPED_CLI_PROVIDER_ID: &str = "scoped-cli-mcp";
#[derive(Debug, Clone)]
pub(crate) struct ScopedMcpRouteContext {
    pub scope_id: String,
    pub owner_id: String,
    pub session_id: String,
}

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

pub(crate) fn handle_scoped_mcp_json_rpc_for_context(
    plugins: &PluginEndpoints<'_>,
    request: Value,
    context: ScopedMcpRouteContext,
) -> crate::Result<Option<Value>> {
    handle_scoped_mcp_json_rpc_with_context(plugins, request, Some(&context))
}

fn handle_scoped_mcp_json_rpc_with_context(
    plugins: &PluginEndpoints<'_>,
    request: Value,
    context: Option<&ScopedMcpRouteContext>,
) -> crate::Result<Option<Value>> {
    let request: JsonRpcRequest = decode_rpc_request(request)?;
    if request.id.is_none() {
        handle_notification(plugins, &request.method, request.params, context)?;
        return Ok(None);
    }
    let id = request
        .id
        .ok_or_else(|| AppCoreError::missing_value("id", "scoped MCP request id"))?;
    let result = dispatch_scoped_mcp_method(plugins, &id, &request.method, request.params, context);
    encode_rpc_response(response_from_result(Some(id), result))
}

fn dispatch_scoped_mcp_method(
    plugins: &PluginEndpoints<'_>,
    request_id: &Value,
    method: &str,
    params: Option<Value>,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<Value, JsonRpcError> {
    match method {
        "initialize" => Ok(initialize_result()),
        "tools/list" => tools_list(plugins, context),
        "tools/call" => call_tool(plugins, request_id, params, context),
        other => Err(method_not_found(other)),
    }
}

fn tools_list(
    plugins: &PluginEndpoints<'_>,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<Value, JsonRpcError> {
    let catalog = scoped_tool_catalog(plugins, context)?;
    Ok(json!({
        "tools": catalog
            .tools()
            .iter()
            .map(tool_descriptor)
            .collect::<Vec<_>>()
    }))
}

fn call_tool(
    plugins: &PluginEndpoints<'_>,
    request_id: &Value,
    params: Option<Value>,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<Value, JsonRpcError> {
    let call = parse_tool_call(params)?;
    let route = tool_route(plugins, &call.name, &call.arguments, context)?;
    let catalog = scoped_tool_catalog(plugins, context)?;
    let advertised_name = route.wire_tool_name.clone();
    let outcome = catalog
        .invoker()
        .invoke_bound(&advertised_name, call.arguments, |arguments| {
            invoke_scoped_bound_tool(plugins, request_id, route, arguments, context)
        });
    tool_outcome_result(outcome)
}

fn scoped_tool_catalog(
    plugins: &PluginEndpoints<'_>,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<McpToolCatalog, JsonRpcError> {
    let scope_id = context
        .map(|context| context.scope_id.as_str())
        .ok_or_else(|| invalid_params("missing scoped MCP route context"))?;
    let tools = plugins
        .scoped_mcp_tools(scope_id)
        .map_err(tool_error)?
        .into_iter()
        .map(ScopedMcpToolRoute::from)
        .filter(|route| route_visible_for_context(route, context))
        .map(|route| McpTool {
            name: route.wire_tool_name,
            description: route.description,
            input_schema: input_schema_for_route(route.input_schema, context),
        })
        .collect();
    McpToolCatalog::from_bound_tools(SCOPED_CLI_PROVIDER_ID, tools)
        .map_err(|error| invalid_params(error.to_string()))
}

fn invoke_scoped_bound_tool(
    plugins: &PluginEndpoints<'_>,
    request_id: &Value,
    route: ScopedMcpToolRoute,
    arguments: Value,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<Value, String> {
    let request = message_request(route, arguments, context).map_err(|error| error.message)?;
    let context = context.ok_or_else(|| "missing scoped MCP route context".to_string())?;
    let scope_id = request.scope_id.clone();
    let invocation = scoped_mcp_channel::plugin_invocation_from_message(request)
        .map_err(|error| error.to_string())?;
    let response = plugins
        .invoke_scoped_mcp_tool_controlled(
            &scope_id,
            invocation,
            scoped_lifecycle(request_id, context),
        )
        .map_err(|error| tool_error(error).message)?;
    match response.status {
        PluginInvocationStatus::Failed => Err(response.output.to_string()),
        PluginInvocationStatus::Completed | PluginInvocationStatus::Accepted => Ok(response.output),
    }
}

fn tool_route(
    plugins: &PluginEndpoints<'_>,
    name: &str,
    arguments: &Value,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<ScopedMcpToolRoute, JsonRpcError> {
    let plugin_id = match context {
        Some(_) => None,
        None => Some(string_argument(arguments, "plugin_id")?),
    };
    let scope_id = context
        .map(|context| context.scope_id.as_str())
        .ok_or_else(|| invalid_params("missing scoped MCP route context"))?;
    plugins
        .scoped_mcp_tools(scope_id)
        .map_err(tool_error)?
        .into_iter()
        .map(ScopedMcpToolRoute::from)
        .filter(|route| route_visible_for_context(route, context))
        .find(|route| route_matches(route, name, plugin_id))
        .ok_or_else(|| invalid_params(format!("unknown scoped MCP tool {name:?}")))
}

fn message_request(
    route: ScopedMcpToolRoute,
    arguments: Value,
    context: Option<&ScopedMcpRouteContext>,
) -> std::result::Result<ScopedMcpMessageRequest, JsonRpcError> {
    let plugin_id = match context {
        Some(_) => Ok(route.plugin_id.clone()),
        None => string_argument(&arguments, "plugin_id").map(str::to_string),
    }?;
    let session_id = context
        .map(|context| context.session_id.clone())
        .map(Ok)
        .unwrap_or_else(|| string_argument(&arguments, "session_id").map(str::to_string))?;
    if plugin_id != route.plugin_id {
        return Err(invalid_params(format!(
            "plugin_id {plugin_id:?} does not own tool"
        )));
    }
    Ok(ScopedMcpMessageRequest {
        scope_id: context
            .map(|context| context.scope_id.clone())
            .ok_or_else(|| invalid_params("missing scoped MCP route context"))?,
        owner_id: context
            .map(|context| context.owner_id.clone())
            .ok_or_else(|| invalid_params("missing scoped MCP route owner"))?,
        plugin_id,
        session_id,
        tool_name: route.tool_name,
        arguments,
    })
}

fn scoped_lifecycle(
    request_id: &Value,
    context: &ScopedMcpRouteContext,
) -> PluginInvocationLifecycle {
    PluginInvocationLifecycle {
        request_id: request_key(request_id),
        owner_id: context.owner_id.clone(),
        session_id: context.session_id.clone(),
        scope_id: Some(context.scope_id.clone()),
        deadline: Instant::now() + Duration::from_secs(60),
    }
}

fn handle_notification(
    plugins: &PluginEndpoints<'_>,
    method: &str,
    params: Option<Value>,
    context: Option<&ScopedMcpRouteContext>,
) -> crate::Result<()> {
    if method != "notifications/cancelled" {
        return Ok(());
    }
    tracing::warn!(target: "debug_a4f2",
        "[DEBUG-a4f2] scoped MCP cancellation notice received: {}",
        serde_json::to_string(&params).unwrap_or_default());
    let context =
        context.ok_or_else(|| AppCoreError::missing_value("route", "scoped MCP route"))?;
    let request_id = params
        .and_then(|params| params.get("requestId").cloned())
        .ok_or_else(|| AppCoreError::missing_value("requestId", "MCP cancellation identity"))?;
    tracing::warn!(target: "debug_a4f2",
        "[DEBUG-a4f2] cancelling scoped invocation request_id={}",
        request_key(&request_id));
    let _ = plugins
        .app
        .plugin_system()
        .cancel_controlled(&PluginInvocationCancellationRequest {
            plugin_id: None,
            request_id: request_key(&request_id),
            owner_id: context.owner_id.clone(),
            session_id: Some(context.session_id.clone()),
            scope_id: Some(context.scope_id.clone()),
        });
    Ok(())
}

fn request_key(request_id: &Value) -> String {
    serde_json::to_string(request_id).unwrap_or_else(|_| request_id.to_string())
}

fn tool_descriptor(tool: &McpTool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
    })
}

fn input_schema_for_route(input_schema: Value, context: Option<&ScopedMcpRouteContext>) -> Value {
    if context.is_none() {
        return input_schema;
    }
    scoped_input_schema(input_schema)
}

fn scoped_input_schema(mut input_schema: Value) -> Value {
    if let Some(object) = input_schema.as_object_mut() {
        strip_hidden_required(object);
        strip_hidden_properties(object);
    }
    input_schema
}

fn strip_hidden_required(object: &mut serde_json::Map<String, Value>) {
    if let Some(Value::Array(required)) = object.get_mut("required") {
        required.retain(|item| {
            !matches!(
                item.as_str(),
                Some("mcp_owner_id" | "plugin_id" | "session_id")
            )
        });
    }
}

fn strip_hidden_properties(object: &mut serde_json::Map<String, Value>) {
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        properties.remove("plugin_id");
        properties.remove("session_id");
        properties.remove("mcp_owner_id");
    }
}

fn route_visible_for_context(
    _route: &ScopedMcpToolRoute,
    context: Option<&ScopedMcpRouteContext>,
) -> bool {
    context.is_some()
}

fn route_matches(route: &ScopedMcpToolRoute, name: &str, plugin_id: Option<&str>) -> bool {
    plugin_id.is_none_or(|plugin_id| route.plugin_id == plugin_id)
        && (route.wire_tool_name == name || route.tool_name == name)
}

fn parse_tool_call(params: Option<Value>) -> std::result::Result<ToolCallParams, JsonRpcError> {
    let params = params.ok_or_else(|| invalid_params("missing tools/call params"))?;
    serde_json::from_value(params).map_err(|error| invalid_params(error.to_string()))
}

fn string_argument<'value>(
    value: &'value Value,
    key: &str,
) -> std::result::Result<&'value str, JsonRpcError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| invalid_params(format!("missing string argument {key:?}")))
}

fn tool_outcome_result(outcome: ToolOutcome) -> std::result::Result<Value, JsonRpcError> {
    match outcome {
        ToolOutcome::Success(payload) => tool_result(payload),
        ToolOutcome::ToolError(error) => recoverable_tool_error(error),
        ToolOutcome::Validation(message) => recoverable_tool_error(json!({ "message": message })),
        ToolOutcome::Transport(error) => {
            recoverable_tool_error(json!({ "message": error.to_string() }))
        }
    }
}

fn tool_result(payload: Value) -> std::result::Result<Value, JsonRpcError> {
    mcp_tool_result(payload, false)
}

fn recoverable_tool_error(payload: Value) -> std::result::Result<Value, JsonRpcError> {
    mcp_tool_result(payload, true)
}

fn mcp_tool_result(payload: Value, is_error: bool) -> std::result::Result<Value, JsonRpcError> {
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
        AppCoreError::invalid_value(error.to_string(), "scoped MCP JSON-RPC request")
    })
}

fn encode_rpc_response(response: JsonRpcResponse) -> crate::Result<Option<Value>> {
    serde_json::to_value(response).map(Some).map_err(|error| {
        AppCoreError::invalid_value(error.to_string(), "scoped MCP JSON-RPC response")
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
