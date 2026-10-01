use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    McpApplication, McpApplicationError, McpCoreError, McpInvocationContext,
    McpInvocationFailureKind, Result,
};

const DEFAULT_INVOCATION_TIMEOUT: Duration = Duration::from_secs(60);
static NEXT_SERVER_ID: AtomicU64 = AtomicU64::new(1);

/// Handles MCP JSON-RPC requests through one application-owned invocation seam.
pub struct LumviseMcpServer {
    application: Arc<dyn McpApplication>,
    owner_id: String,
    session_id: String,
    invocation_timeout: Duration,
    in_flight: Mutex<HashMap<String, McpInvocationContext>>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelledParams {
    request_id: Value,
}

impl LumviseMcpServer {
    /// Creates a JSON-RPC server over an application adapter.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use lumvise_mcp_core::{
    ///     LumviseMcpServer, McpApplication, McpApplicationError, McpTool,
    /// };
    /// use serde_json::Value;
    ///
    /// struct EmptyApplication;
    /// impl McpApplication for EmptyApplication {
    ///     fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError> {
    ///         Ok(Vec::new())
    ///     }
    ///     fn invoke_tool(
    ///         &self,
    ///         name: &str,
    ///         _arguments: Value,
    ///     ) -> Result<Value, McpApplicationError> {
    ///         Err(McpApplicationError::invalid_params(format!(
    ///             "unknown tool {name:?}"
    ///         )))
    ///     }
    /// }
    /// let _server = LumviseMcpServer::new(Arc::new(EmptyApplication));
    /// ```
    pub fn new(application: Arc<dyn McpApplication>) -> Self {
        let identity = NEXT_SERVER_ID.fetch_add(1, Ordering::Relaxed);
        Self::with_identity(
            application,
            format!("mcp-owner-{}-{identity}", std::process::id()),
            format!("mcp-session-{}-{identity}", std::process::id()),
        )
    }

    /// Creates a server with explicit stable owner and session identities.
    pub fn with_identity(
        application: Arc<dyn McpApplication>,
        owner_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            application,
            owner_id: owner_id.into(),
            session_id: session_id.into(),
            invocation_timeout: DEFAULT_INVOCATION_TIMEOUT,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Handles one newline-delimited MCP JSON-RPC message.
    pub fn handle_json_line(&self, line: &str) -> Result<Option<String>> {
        let request: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => return Ok(Some(parse_error_response(error))),
        };
        if request.id.is_none() {
            self.handle_notification(request)?;
            return Ok(None);
        }
        let response = self.handle_request(request);
        Ok(Some(serde_json::to_string(&response)?))
    }

    pub(crate) fn classify_json_line(&self, line: &str) -> crate::stdio::StdioTrafficClass {
        let method = serde_json::from_str::<Value>(line).ok().and_then(|value| {
            value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        match method.as_deref() {
            Some("tools/call") => crate::stdio::StdioTrafficClass::Invocation,
            Some("notifications/cancelled") => crate::stdio::StdioTrafficClass::Cancellation,
            _ => crate::stdio::StdioTrafficClass::Control,
        }
    }

    pub(crate) fn prepare_json_line(&self, line: &str) -> Result<()> {
        let Ok(request) = serde_json::from_str::<JsonRpcRequest>(line) else {
            return Ok(());
        };
        if request.method != "tools/call" {
            return Ok(());
        }
        let Some(id) = request.id else {
            return Ok(());
        };
        self.register_invocation(self.invocation_context(&id))
    }

    pub(crate) fn reject_json_line(&self, line: &str) {
        let id = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value.get("id").cloned());
        if let Some(id) = id {
            self.remove_invocation(&request_key(&id));
        }
    }

    pub(crate) fn overload_json_line(&self, line: &str) -> Option<String> {
        let id = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value.get("id").cloned());
        id.map(|id| {
            serde_json::to_string(&JsonRpcResponse::failure(
                Some(id),
                JsonRpcError {
                    code: -32001,
                    message: "MCP invocation queue is full".into(),
                    data: Some(json!({"kind": "busy", "retryable": true})),
                },
            ))
            .expect("JSON-RPC overload response serializes")
        })
    }

    fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id;
        let result = match request.method.as_str() {
            "initialize" => Ok(initialize_result()),
            "tools/list" => self.tools_list(),
            "tools/call" => {
                self.call_tool(id.as_ref().expect("request id checked"), request.params)
            }
            method => Err(method_not_found(method)),
        };
        response_from_result(id, result)
    }

    fn tools_list(&self) -> std::result::Result<Value, JsonRpcError> {
        let tools = self.application.list_tools().map_err(application_error)?;
        Ok(json!({ "tools": tools }))
    }

    fn call_tool(
        &self,
        id: &Value,
        params: Option<Value>,
    ) -> std::result::Result<Value, JsonRpcError> {
        let call = match parse_tool_call(params) {
            Ok(call) => call,
            Err(error) => {
                self.remove_invocation(&request_key(id));
                return Err(error);
            }
        };
        let context = match self.registered_invocation(id) {
            Ok(Some(context)) => context,
            Ok(None) => {
                let context = self.invocation_context(id);
                if let Err(error) = self.register_invocation(context.clone()) {
                    return Err(internal_error(error.to_string()));
                }
                context
            }
            Err(error) => return Err(internal_error(error.to_string())),
        };
        let result = self
            .application
            .invoke_tool_controlled(&context, &call.name, call.arguments)
            .map_err(application_error);
        self.remove_invocation(context.request_id());
        let payload = result?;
        tool_result(payload)
    }

    fn handle_notification(&self, request: JsonRpcRequest) -> Result<()> {
        if request.method != "notifications/cancelled" {
            return Ok(());
        }
        let params = request.params.ok_or_else(|| {
            McpCoreError::invalid_request("missing", "notifications/cancelled params")
        })?;
        let cancelled: CancelledParams = serde_json::from_value(params)?;
        self.cancel_invocation(&cancelled.request_id)
    }

    fn invocation_context(&self, id: &Value) -> McpInvocationContext {
        McpInvocationContext::with_timeout(
            request_key(id),
            self.owner_id.clone(),
            self.session_id.clone(),
            None,
            self.invocation_timeout,
        )
    }

    fn register_invocation(&self, context: McpInvocationContext) -> Result<()> {
        let mut in_flight = self.lock_in_flight()?;
        if in_flight
            .insert(context.request_id().to_owned(), context.clone())
            .is_some()
        {
            return Err(McpCoreError::invalid_request(
                context.request_id(),
                "unique in-flight JSON-RPC request id",
            ));
        }
        Ok(())
    }

    fn registered_invocation(&self, id: &Value) -> Result<Option<McpInvocationContext>> {
        Ok(self.lock_in_flight()?.get(&request_key(id)).cloned())
    }

    fn remove_invocation(&self, request_id: &str) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            in_flight.remove(request_id);
        }
    }

    fn cancel_invocation(&self, id: &Value) -> Result<()> {
        let context = self.lock_in_flight()?.get(&request_key(id)).cloned();
        let Some(context) = context else {
            return Ok(());
        };
        context.cancellation().cancel();
        let _ = self.application.cancel_invocation(&context);
        Ok(())
    }

    fn lock_in_flight(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, McpInvocationContext>>> {
        self.in_flight.lock().map_err(|_| {
            McpCoreError::invalid_request("poisoned in-flight registry", "available MCP registry")
        })
    }
}

fn request_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| id.to_string())
}

fn parse_error_response(error: serde_json::Error) -> String {
    serde_json::to_string(&JsonRpcResponse::failure(
        Some(Value::Null),
        JsonRpcError {
            code: -32700,
            message: format!("JSON-RPC parse error: {error}"),
            data: None,
        },
    ))
    .expect("JSON-RPC parse error response serializes")
}

fn parse_tool_call(params: Option<Value>) -> std::result::Result<ToolCallParams, JsonRpcError> {
    let params = params.ok_or_else(|| invalid_params("missing tools/call params"))?;
    serde_json::from_value(params).map_err(|error| invalid_params(error.to_string()))
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

fn tool_result(payload: Value) -> std::result::Result<Value, JsonRpcError> {
    let text =
        serde_json::to_string(&payload).map_err(|error| invalid_params(error.to_string()))?;
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": { "name": "lumvise-mcp-core", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "tools": {} }
    })
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

fn internal_error(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: -32603,
        message: message.into(),
        data: None,
    }
}

fn application_error(error: McpApplicationError) -> JsonRpcError {
    match error {
        McpApplicationError::InvalidParams(message) => invalid_params(message),
        McpApplicationError::Invocation(message) => JsonRpcError {
            code: -32000,
            message,
            data: None,
        },
        McpApplicationError::ControlledInvocation {
            kind,
            message,
            retryable,
        } => JsonRpcError {
            code: controlled_error_code(kind),
            message,
            data: Some(json!({ "kind": controlled_error_name(kind), "retryable": retryable })),
        },
    }
}

fn controlled_error_code(kind: McpInvocationFailureKind) -> i64 {
    match kind {
        McpInvocationFailureKind::Busy => -32001,
        McpInvocationFailureKind::DeadlineExceeded => -32002,
        McpInvocationFailureKind::Cancelled => -32003,
        McpInvocationFailureKind::Unavailable => -32004,
        McpInvocationFailureKind::Failed => -32000,
    }
}

fn controlled_error_name(kind: McpInvocationFailureKind) -> &'static str {
    match kind {
        McpInvocationFailureKind::Busy => "busy",
        McpInvocationFailureKind::DeadlineExceeded => "deadline_exceeded",
        McpInvocationFailureKind::Cancelled => "cancelled",
        McpInvocationFailureKind::Unavailable => "unavailable",
        McpInvocationFailureKind::Failed => "failed",
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::McpTool;

    struct CancellableApplication {
        entered: mpsc::Sender<()>,
    }

    impl McpApplication for CancellableApplication {
        fn list_tools(&self) -> std::result::Result<Vec<McpTool>, McpApplicationError> {
            Ok(Vec::new())
        }

        fn invoke_tool(
            &self,
            _name: &str,
            _arguments: Value,
        ) -> std::result::Result<Value, McpApplicationError> {
            unreachable!("server uses controlled invocation")
        }

        fn invoke_tool_controlled(
            &self,
            context: &McpInvocationContext,
            _name: &str,
            _arguments: Value,
        ) -> std::result::Result<Value, McpApplicationError> {
            self.entered.send(()).expect("signal entry");
            let deadline = Instant::now() + Duration::from_secs(1);
            while !context.cancellation().is_cancelled() {
                assert!(Instant::now() < deadline, "cancellation not delivered");
                thread::yield_now();
            }
            Err(McpApplicationError::controlled_invocation(
                McpInvocationFailureKind::Cancelled,
                "cancelled",
                false,
            ))
        }
    }

    #[test]
    fn cancellation_notification_reaches_matching_live_request() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let server = Arc::new(LumviseMcpServer::with_identity(
            Arc::new(CancellableApplication {
                entered: entered_tx,
            }),
            "owner-a",
            "session-a",
        ));
        let invoking = Arc::clone(&server);
        let call = thread::spawn(move || {
            invoking.handle_json_line(
                r#"{"jsonrpc":"2.0","id":"request-a","method":"tools/call","params":{"name":"run","arguments":{}}}"#,
            )
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("call entered");

        let notification = server.handle_json_line(
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"request-a"}}"#,
        ).expect("cancel notification");
        let response = call
            .join()
            .expect("call join")
            .expect("call response")
            .expect("response");
        let response: Value = serde_json::from_str(&response).unwrap();

        assert!(notification.is_none());
        assert_eq!(response["error"]["data"]["kind"], "cancelled");
        assert_eq!(response["error"]["code"], -32003);
    }
}
