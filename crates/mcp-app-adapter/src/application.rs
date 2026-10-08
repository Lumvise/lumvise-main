use crate::session_project_binding::SessionProjectBinding;
use crate::{app_bridge::AppBridgeRuntime, tool_catalog::tool_catalog};
use lumvise_mcp_core::{
    AppBridgeInvocationResponseV1, AppBridgeInvocationStatusV1, LumviseMcpServer, McpApplication,
    McpApplicationError, McpInvocationContext, McpInvocationFailureKind, McpTool,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Configures the single desktop app bridge used by the MCP transport.
pub struct McpAppConfig {
    app_bridge: AppBridgeConfig,
    native_assistant_caller: NativeAssistantCaller,
    initial_project_root: Option<String>,
}

const ASSISTANT_PLUGIN_ID: &str = "builtin.assistant";
const ASSISTANT_SCOPE_ID: &str = "assistant_session";
const START_ASSISTANT_TOOL: &str = "app_plugin.builtin.assistant.start_assistant_session";

#[derive(Clone)]
struct NativeAssistantCaller {
    engine: String,
    instance_id: String,
}

#[derive(Clone)]
struct BoundAssistantSession {
    session_id: String,
    session_epoch: u64,
    active_tools: Vec<String>,
}

/// Routes MCP discovery and invocation to the desktop-owned App Core.
struct LumviseMcpApplication {
    app_bridge: AppBridgeRuntime,
    native_assistant_caller: NativeAssistantCaller,
    bound_assistant_session: Mutex<Option<BoundAssistantSession>>,
    project_binding: SessionProjectBinding,
}

pub use crate::app_bridge::AppBridgeConfig;

#[derive(Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl McpAppConfig {
    /// Uses the canonical discovery marker for the running desktop app.
    ///
    /// # Example
    ///
    /// ```
    /// let _config = lumvise_mcp_app_adapter::McpAppConfig::discovery();
    /// ```
    pub fn discovery() -> Self {
        Self {
            app_bridge: AppBridgeConfig::discovery(),
            native_assistant_caller: default_native_assistant_caller(),
            initial_project_root: None,
        }
    }

    /// Uses an explicit bridge configuration for an isolated runtime root.
    ///
    /// # Example
    ///
    /// ```
    /// let bridge = lumvise_mcp_app_adapter::AppBridgeConfig::discovery()
    ///     .with_runtime_root("/tmp/lumvise-runtime");
    /// let _config = lumvise_mcp_app_adapter::McpAppConfig::from_app_bridge(bridge);
    /// ```
    pub fn from_app_bridge(app_bridge: AppBridgeConfig) -> Self {
        Self {
            app_bridge,
            native_assistant_caller: default_native_assistant_caller(),
            initial_project_root: None,
        }
    }

    pub fn with_native_assistant_caller(
        mut self,
        engine: impl Into<String>,
        instance_id: impl Into<String>,
    ) -> Self {
        self.native_assistant_caller = NativeAssistantCaller {
            engine: engine.into(),
            instance_id: instance_id.into(),
        };
        self
    }

    /// Selects the initial explicit binding; e.g. `.with_project_root("/work/repo")`.
    /// It is published only after the client completes initialization.
    pub fn with_project_root(mut self, project_root: impl Into<String>) -> Self {
        self.initial_project_root = Some(project_root.into());
        self
    }
}

impl Default for McpAppConfig {
    fn default() -> Self {
        Self::discovery()
    }
}

fn default_native_assistant_caller() -> NativeAssistantCaller {
    NativeAssistantCaller {
        engine: "codex".into(),
        instance_id: format!("lumvise-mcp-{}", std::process::id()),
    }
}

/// Opens a generic MCP server that routes through one desktop app bridge.
///
/// # Example
///
/// ```
/// let _server = lumvise_mcp_app_adapter::open_server(
///     lumvise_mcp_app_adapter::McpAppConfig::discovery(),
/// );
/// ```
pub fn open_server(config: McpAppConfig) -> LumviseMcpServer {
    let owner_id = config.native_assistant_caller.instance_id.clone();
    let application = LumviseMcpApplication {
        project_binding: SessionProjectBinding::new(
            config.app_bridge.clone(),
            config.initial_project_root,
        ),
        app_bridge: AppBridgeRuntime::start(config.app_bridge),
        native_assistant_caller: config.native_assistant_caller,
        bound_assistant_session: Mutex::new(None),
    };
    LumviseMcpServer::with_identity(Arc::new(application), owner_id.clone(), owner_id)
}

impl LumviseMcpApplication {
    fn tools_list(&self) -> std::result::Result<Vec<McpTool>, JsonRpcError> {
        let mut bridge_tools = self.app_bridge.plugin_tools().map_err(tool_error)?;
        for tool in &mut bridge_tools {
            expose_session_driver(tool);
        }
        if bridge_tools
            .iter()
            .any(|tool| tool["name"] == START_ASSISTANT_TOOL)
        {
            let mut scoped_tools = self
                .app_bridge
                .scoped_plugin_tools(ASSISTANT_SCOPE_ID)
                .unwrap_or_default();
            for tool in &mut scoped_tools {
                remove_bound_session_fields(tool);
            }
            bridge_tools.extend(scoped_tools);
        }
        tool_catalog(&bridge_tools).map_err(tool_error)
    }

    fn bound_assistant_session(
        &self,
    ) -> std::result::Result<Option<BoundAssistantSession>, JsonRpcError> {
        self.bound_assistant_session
            .lock()
            .map(|bound| bound.clone())
            .map_err(|_| tool_error("bound Assistant session lock is poisoned"))
    }

    fn dispatch_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, JsonRpcError> {
        match name {
            "discover_app_plugins" => self.discover_app_plugins(),
            "invoke_app_plugin_capability" => Err(tool_error("controlled invocation required")),
            "app_bridge_status" => self.app_bridge_status(),
            "set_current_project" => {
                self.project_binding
                    .select(arguments)
                    .map_err(|error| match error {
                        McpApplicationError::InvalidParams(message) => invalid_params(message),
                        other => tool_error(other),
                    })
            }
            other if other.starts_with("app_plugin.") => {
                Err(tool_error("controlled invocation required"))
            }
            other => Err(invalid_params(format!("unknown tool {other:?}"))),
        }
    }

    fn discover_app_plugins(&self) -> std::result::Result<Value, JsonRpcError> {
        self.app_bridge.discover_plugins().map_err(tool_error)
    }

    fn app_bridge_status(&self) -> std::result::Result<Value, JsonRpcError> {
        Ok(self.app_bridge.status())
    }

    fn invoke_started_assistant(
        &self,
        context: &McpInvocationContext,
        arguments: Value,
    ) -> std::result::Result<Value, McpApplicationError> {
        let mut input = arguments.as_object().cloned().ok_or_else(|| {
            McpApplicationError::invalid_params("Assistant start input must be an object")
        })?;
        select_session_driver(&mut input, &self.native_assistant_caller)?;
        input.insert(
            "mcp_owner_id".into(),
            Value::String(self.native_assistant_caller.instance_id.clone()),
        );
        let response = self
            .app_bridge
            .invoke_dynamic_tool_controlled(context, START_ASSISTANT_TOOL, Value::Object(input))
            .map_err(McpApplicationError::invocation)?;
        let output = decode_controlled_response(response)?;
        let session_id = output
            .pointer("/state/session_id")
            .and_then(Value::as_str)
            .filter(|session_id| !session_id.trim().is_empty())
            .ok_or_else(|| {
                McpApplicationError::invocation(format!(
                    "invalid Assistant start output {output}; expected state.session_id"
                ))
            })?
            .to_owned();
        let active_tools = output
            .get("active_tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                McpApplicationError::invocation(format!(
                    "invalid Assistant start output {output}; expected active_tools"
                ))
            })?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let session_epoch = output
            .pointer("/state/session_epoch")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                McpApplicationError::invocation(format!(
                    "invalid Assistant start output {output}; expected unsigned state.session_epoch"
                ))
            })?;
        *self.bound_assistant_session.lock().map_err(|_| {
            McpApplicationError::invocation("bound Assistant session lock is poisoned")
        })? = Some(BoundAssistantSession {
            session_id,
            session_epoch,
            active_tools,
        });
        Ok(output)
    }

    fn invoke_bound_assistant_tool(
        &self,
        context: &McpInvocationContext,
        name: &str,
        arguments: Value,
    ) -> std::result::Result<Option<Value>, McpApplicationError> {
        let generic = name == "invoke_app_plugin_capability";
        let name = if generic {
            arguments["capability_id"].as_str().unwrap_or(name)
        } else {
            name
        };
        let Some(bound) = self
            .bound_assistant_session()
            .map_err(mcp_application_error)?
        else {
            return Ok(None);
        };
        let requested_plugin = generic.then(|| arguments["plugin_id"].as_str()).flatten();
        let Some((plugin_id, capability_id)) =
            self.bound_tool_route(&bound, name, requested_plugin)?
        else {
            return Ok(None);
        };
        let scoped_arguments = if generic {
            &arguments["input"]
        } else {
            &arguments
        };
        let mut input = scoped_arguments.as_object().cloned().ok_or_else(|| {
            McpApplicationError::invalid_params(format!(
                "Assistant tool {name:?} input must be an object"
            ))
        })?;
        input.insert("plugin_id".into(), Value::String(plugin_id.clone()));
        input.insert("session_id".into(), Value::String(bound.session_id.clone()));
        if plugin_id == ASSISTANT_PLUGIN_ID {
            // Retain the start generation: querying current state here would let
            // an old caller write into a conversation reopened by another caller.
            input.insert("session_epoch".into(), Value::from(bound.session_epoch));
        }
        let response = self
            .app_bridge
            .invoke_scoped_tool_controlled(
                context,
                &plugin_id,
                &capability_id,
                &bound.session_id,
                ASSISTANT_SCOPE_ID,
                Value::Object(input),
                true,
                Duration::from_secs(55),
            )
            .map_err(McpApplicationError::invocation)?;
        let output = decode_controlled_response(response)?;
        if name == "assistant.finish" {
            *self.bound_assistant_session.lock().map_err(|_| {
                McpApplicationError::invocation("bound Assistant session lock is poisoned")
            })? = None;
        }
        Ok(Some(output))
    }

    fn bound_tool_route(
        &self,
        bound: &BoundAssistantSession,
        name: &str,
        requested_plugin: Option<&str>,
    ) -> std::result::Result<Option<(String, String)>, McpApplicationError> {
        if bound.active_tools.iter().any(|active| active == name)
            && requested_plugin.is_none_or(|plugin| plugin == ASSISTANT_PLUGIN_ID)
        {
            return Ok(Some((ASSISTANT_PLUGIN_ID.into(), name.into())));
        }
        if name.starts_with("app_plugin.")
            || !name.contains('.')
            || name.starts_with("assistant.")
            || name.starts_with("canvas.")
        {
            return Ok(None);
        }
        self.app_bridge
            .scoped_tool_owner(ASSISTANT_SCOPE_ID, name, requested_plugin)
            .map_err(McpApplicationError::invocation)
    }
}

fn select_session_driver(
    input: &mut serde_json::Map<String, Value>,
    caller: &NativeAssistantCaller,
) -> std::result::Result<(), McpApplicationError> {
    let native = match input.remove("in_session") {
        None | Some(Value::Bool(true)) => true,
        Some(Value::Bool(false)) => false,
        Some(value) => {
            return Err(McpApplicationError::invalid_params(format!(
                "invalid in_session {value}; expected boolean"
            )));
        }
    };
    input.remove("native_llm_caller");
    if native {
        input.insert(
            "native_llm_caller".into(),
            serde_json::json!({
                "engine":caller.engine,"instance_id":caller.instance_id
            }),
        );
    }
    Ok(())
}

fn expose_session_driver(tool: &mut Value) {
    if tool["name"] != START_ASSISTANT_TOOL {
        return;
    }
    let policy = tool["description"].as_str().unwrap_or_default();
    tool["description"] = Value::String(format!(
        "{policy} in_session=true: you drive turns; false: the configured app engine drives while you observe and answer background-context questions."
    ));
    tool["inputSchema"]["properties"]["in_session"] = serde_json::json!({
        "type":"boolean","default":true,
        "description":"Whether the calling LLM produces the conversation turns."
    });
    if let Some(properties) = tool["inputSchema"]["properties"].as_object_mut() {
        properties.remove("native_llm_caller");
        properties.remove("mcp_owner_id");
    }
}

impl McpApplication for LumviseMcpApplication {
    fn mcp_client_initialized(&self) -> Result<(), McpApplicationError> {
        self.project_binding
            .initialize()
            .map_err(McpApplicationError::invocation)
    }

    fn mcp_client_disconnected(&self) {
        self.project_binding.close();
    }
    fn list_tools(&self) -> std::result::Result<Vec<McpTool>, McpApplicationError> {
        self.tools_list().map_err(mcp_application_error)
    }

    fn invoke_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, McpApplicationError> {
        self.dispatch_tool(name, arguments)
            .map_err(mcp_application_error)
    }

    fn invoke_tool_controlled(
        &self,
        context: &McpInvocationContext,
        name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, McpApplicationError> {
        if name == START_ASSISTANT_TOOL {
            return self.invoke_started_assistant(context, arguments);
        }
        if let Some(output) = self.invoke_bound_assistant_tool(context, name, arguments.clone())? {
            return Ok(output);
        }
        let response = match name {
            "invoke_app_plugin_capability" => self
                .app_bridge
                .invoke_capability_controlled(context, arguments),
            other if other.starts_with("app_plugin.") => self
                .app_bridge
                .invoke_dynamic_tool_controlled(context, other, arguments),
            _ => return self.invoke_tool(name, arguments),
        }
        .map_err(McpApplicationError::invocation)?;
        decode_controlled_response(response)
    }

    fn cancel_invocation(
        &self,
        context: &McpInvocationContext,
    ) -> std::result::Result<(), McpApplicationError> {
        context.cancellation().cancel();
        self.app_bridge
            .cancel_controlled(context)
            .map(|_| ())
            .map_err(McpApplicationError::invocation)
    }
}

fn remove_bound_session_fields(tool: &mut Value) {
    let response_tool = tool["name"] == "assistant.respond";
    let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.remove("plugin_id");
        properties.remove("session_id");
        properties.remove("session_epoch");
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|field| {
            !matches!(
                field.as_str(),
                Some("plugin_id" | "session_id" | "session_epoch")
            )
        });
    }
    if response_tool {
        schema
            .entry("properties")
            .or_insert_with(|| serde_json::json!({}))["turn_id"] = serde_json::json!({
            "type":"string","description":"Required for native caller responses: current turn_id returned by session start or assistant.await_turn."
        });
    }
}

fn decode_controlled_response(
    response: AppBridgeInvocationResponseV1,
) -> std::result::Result<Value, McpApplicationError> {
    let status = AppBridgeInvocationStatusV1::try_from(response.status)
        .unwrap_or(AppBridgeInvocationStatusV1::Internal);
    if status == AppBridgeInvocationStatusV1::Completed {
        return serde_json::from_slice(&response.output_json)
            .map_err(|error| McpApplicationError::invocation(error.to_string()));
    }
    Err(McpApplicationError::controlled_invocation(
        failure_kind(status),
        response.message,
        response.retryable,
    ))
}

fn failure_kind(status: AppBridgeInvocationStatusV1) -> McpInvocationFailureKind {
    match status {
        AppBridgeInvocationStatusV1::Busy => McpInvocationFailureKind::Busy,
        AppBridgeInvocationStatusV1::DeadlineExceeded => McpInvocationFailureKind::DeadlineExceeded,
        AppBridgeInvocationStatusV1::Cancelled => McpInvocationFailureKind::Cancelled,
        AppBridgeInvocationStatusV1::Unavailable => McpInvocationFailureKind::Unavailable,
        AppBridgeInvocationStatusV1::Completed
        | AppBridgeInvocationStatusV1::Failed
        | AppBridgeInvocationStatusV1::Internal => McpInvocationFailureKind::Failed,
    }
}

fn invalid_params(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: -32602,
        message: message.into(),
    }
}

fn tool_error(error: impl std::fmt::Display) -> JsonRpcError {
    JsonRpcError {
        code: -32000,
        message: error.to_string(),
    }
}

fn mcp_application_error(error: JsonRpcError) -> McpApplicationError {
    if error.code == -32602 {
        return McpApplicationError::invalid_params(error.message);
    }
    McpApplicationError::invocation(error.message)
}
