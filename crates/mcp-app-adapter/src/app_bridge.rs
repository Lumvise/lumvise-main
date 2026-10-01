use lumvise_app_core::AppRuntimeCoordinator;
use lumvise_app_core::RuntimeConnection;
use lumvise_mcp_core::app_bridge_transport::{
    AppBridgeHttpMethod, AppBridgeHttpTransport, AppBridgeRequestError,
};
use lumvise_mcp_core::{
    APP_BRIDGE_PROTOCOL_MAJOR, AppBridgeCancellationRequestV1, AppBridgeCancellationResponseV1,
    AppBridgeInvocationRequestV1, AppBridgeInvocationResponseV1, AppBridgeInvocationStatusV1,
    McpInvocationContext,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const RUNTIME_DISCOVERY_FILE: &str = "runtime.json";
mod startup_connection;
use startup_connection::StartupConnection;
#[cfg(test)]
mod startup_tests;
#[derive(Clone)]
pub struct AppBridgeConfig {
    discovery_path: PathBuf,
    coordinator: Arc<AppRuntimeCoordinator>,
    terminal: Arc<AtomicBool>,
}

impl std::fmt::Debug for AppBridgeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppBridgeConfig")
            .field("discovery_path", &self.discovery_path)
            .finish()
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PluginCapability {
    plugin_id: String,
    capability_id: String,
    dynamic_tool_name: Option<String>,
    input_schema: Option<Value>,
    description: Option<String>,
    #[serde(default)]
    availability: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PluginSurface {
    generation: String,
    #[serde(default = "current_freshness")]
    freshness: String,
    #[serde(default)]
    capabilities: Vec<PluginCapability>,
}

fn current_freshness() -> String {
    "current".into()
}

#[derive(Clone)]
struct ActiveTarget {
    plugin_id: String,
    connection: RuntimeConnection,
    session_id: String,
    scope_id: Option<String>,
    interrupt_on_cancel: bool,
}

pub(crate) struct AppBridgeRuntime {
    config: AppBridgeConfig,
    startup: StartupConnection,
    transport: AppBridgeHttpTransport,
    active_targets: Mutex<HashMap<(String, String), ActiveTarget>>,
    next_interrupt_request: AtomicU64,
    last_known_surface: Mutex<Option<PluginSurface>>,
}

impl AppBridgeConfig {
    /// Uses the canonical app-daemon discovery marker.
    ///
    /// # Example
    ///
    /// ```
    /// let config = lumvise_mcp_app_adapter::AppBridgeConfig::discovery();
    pub fn discovery() -> Self {
        let root = AppRuntimeCoordinator::production()
            .runtime_root()
            .to_path_buf();
        Self {
            discovery_path: root.join(RUNTIME_DISCOVERY_FILE),
            coordinator: Arc::new(coordinator_for(root)),
            terminal: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Overrides the app-daemon discovery marker path.
    ///
    /// # Example
    ///
    /// ```
    /// let _config = lumvise_mcp_app_adapter::AppBridgeConfig::discovery()
    ///     .with_discovery_path("/tmp/lumvise-runtime.json");
    /// ```
    pub fn with_discovery_path(mut self, path: impl AsRef<Path>) -> Self {
        self.discovery_path = path.as_ref().to_path_buf();
        self.terminal = Arc::new(AtomicBool::new(false));
        self.coordinator = Arc::new(coordinator_for(self.runtime_root()));
        self
    }

    /// Overrides the private runtime root for tests and isolated harnesses.
    pub fn with_runtime_root(mut self, root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        self.discovery_path = root.join(RUNTIME_DISCOVERY_FILE);
        self.terminal = Arc::new(AtomicBool::new(false));
        self.coordinator = Arc::new(coordinator_for(root));
        self
    }

    pub(crate) fn coordinator(&self) -> &AppRuntimeCoordinator {
        self.coordinator.as_ref()
    }

    pub(crate) fn mark_terminal(&self) {
        self.terminal.store(true, Ordering::Release);
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    pub(crate) fn runtime_root(&self) -> PathBuf {
        self.discovery_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub(crate) fn current_connection(&self) -> std::result::Result<RuntimeConnection, String> {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        self.current_connection_until(
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            &cancelled,
        )
    }

    fn current_connection_until(
        &self,
        deadline: std::time::Instant,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<RuntimeConnection, String> {
        crate::ensure_runtime(self, deadline, cancelled)
    }
}

impl AppBridgeRuntime {
    pub(crate) fn start(config: AppBridgeConfig) -> Self {
        Self {
            startup: StartupConnection::new(config.clone()),
            config,
            transport: AppBridgeHttpTransport::new(),
            active_targets: Mutex::new(HashMap::new()),
            next_interrupt_request: AtomicU64::new(1),
            last_known_surface: Mutex::new(None),
        }
    }

    pub(crate) fn status(&self) -> Value {
        let (connection, error) = match self.startup.probe() {
            Ok(connection) => (connection, None),
            Err(error) => (None, Some(error)),
        };
        json!({
            "linked": connection.is_some(),
            "state": if connection.is_some() { "ready" } else if error.is_some() { "unavailable" } else { "initializing" },
            "error": error,
            "base_url": connection.map(|value| value.app_bridge_base_url),
        })
    }

    pub(crate) fn discover_plugins(&self) -> std::result::Result<Value, String> {
        match self.refresh_surface() {
            Ok(surface) => serde_json::to_value(surface).map_err(|error| error.to_string()),
            Err(error) => self.stale_surface(error),
        }
    }

    pub(crate) fn invoke_dynamic_tool_controlled(
        &self,
        context: &McpInvocationContext,
        tool_name: &str,
        arguments: Value,
    ) -> std::result::Result<AppBridgeInvocationResponseV1, String> {
        let capability = self.capability_for_tool(tool_name)?;
        self.invoke_controlled(context, &capability, invocation_input(arguments))
    }

    pub(crate) fn invoke_capability_controlled(
        &self,
        context: &McpInvocationContext,
        arguments: Value,
    ) -> std::result::Result<AppBridgeInvocationResponseV1, String> {
        let (capability, input) = direct_capability(arguments)?;
        self.invoke_controlled(context, &capability, input)
    }

    pub(crate) fn invoke_scoped_tool_controlled(
        &self,
        context: &McpInvocationContext,
        plugin_id: &str,
        capability_id: &str,
        session_id: &str,
        scope_id: &str,
        input: Value,
        interrupt_on_cancel: bool,
        timeout: std::time::Duration,
    ) -> std::result::Result<AppBridgeInvocationResponseV1, String> {
        let deadline = std::time::SystemTime::now()
            .checked_add(timeout.min(context.remaining()))
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or_else(|| context.deadline_unix_ms());
        let wire_context = McpInvocationContext::new(
            context.request_id(),
            context.owner_id(),
            session_id,
            Some(scope_id.to_owned()),
            deadline,
        );
        self.invoke_controlled_with_identity(
            context,
            &wire_context,
            &PluginCapability {
                plugin_id: plugin_id.to_owned(),
                capability_id: capability_id.to_owned(),
                dynamic_tool_name: None,
                input_schema: None,
                availability: None,
                description: None,
            },
            input,
            interrupt_on_cancel,
        )
    }

    pub(crate) fn cancel_controlled(
        &self,
        context: &McpInvocationContext,
    ) -> std::result::Result<bool, String> {
        let Some(target) = self.active_target(context)? else {
            return Ok(false);
        };
        let request = AppBridgeCancellationRequestV1 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id: context.request_id().to_owned(),
            owner_id: context.owner_id().to_owned(),
            session_id: target.session_id.clone(),
            scope_id: target.scope_id.clone(),
            plugin_id: target.plugin_id.clone(),
        };
        let cancelled = self
            .post_protobuf(
                &target.connection,
                "/api/mcp/plugins/cancel-v1",
                request.encode_to_vec(),
            )
            .and_then(|response| {
                let response = AppBridgeCancellationResponseV1::decode(response.as_slice())
                    .map_err(|error| {
                        format!("invalid app bridge cancellation response: {error}")
                    })?;
                require_protocol(response.protocol_major)?;
                Ok(response.cancelled)
            });
        self.remove_target(context);
        let interrupted = target
            .interrupt_on_cancel
            .then(|| self.interrupt_assistant_session(context, &target))
            .transpose()
            .map(|_| ());
        match (cancelled, interrupted) {
            (Ok(cancelled), Ok(())) => Ok(cancelled),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(cancel_error), Err(interrupt_error)) => Err(format!(
                "{cancel_error}; terminal Assistant interruption also failed: {interrupt_error}"
            )),
        }
    }

    fn interrupt_assistant_session(
        &self,
        context: &McpInvocationContext,
        target: &ActiveTarget,
    ) -> std::result::Result<(), String> {
        let sequence = self.next_interrupt_request.fetch_add(1, Ordering::Relaxed);
        let request = AppBridgeInvocationRequestV1 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id: format!("{}:interrupted:{sequence}", context.request_id()),
            owner_id: context.owner_id().to_owned(),
            session_id: target.session_id.clone(),
            scope_id: None,
            deadline_unix_ms: context.deadline_unix_ms(),
            plugin_id: target.plugin_id.clone(),
            capability_id: "advance_assistant_state".into(),
            input_json: serde_json::to_vec(&json!({
                "session_id": target.session_id.clone(),
                "event": "interrupted",
                "error": "MCP caller cancelled the active Assistant turn"
            }))
            .map_err(|error| error.to_string())?,
        };
        let response = self.post_protobuf_with_timeout(
            &target.connection,
            "/api/mcp/plugins/invoke-v1",
            request.encode_to_vec(),
            context.remaining(),
        )?;
        let response = AppBridgeInvocationResponseV1::decode(response.as_slice())
            .map_err(|error| format!("invalid app bridge interruption response: {error}"))?;
        require_protocol(response.protocol_major)?;
        if AppBridgeInvocationStatusV1::try_from(response.status)
            .unwrap_or(AppBridgeInvocationStatusV1::Internal)
            != AppBridgeInvocationStatusV1::Completed
        {
            return Err(format!(
                "terminal Assistant interruption failed: {}",
                response.message
            ));
        }
        Ok(())
    }

    pub(crate) fn plugin_tools(&self) -> std::result::Result<Vec<Value>, String> {
        // MCP must initialize even before the database-backed app catalog exists.
        // Discovery reports the error; the base status/invocation tools stay usable.
        let surface = self.refresh_surface().ok().or(self.cached_surface()?);
        Ok(surface
            .into_iter()
            .flat_map(|surface| surface.capabilities)
            .map(bridge_tool)
            .collect())
    }
    pub(crate) fn scoped_plugin_tools(
        &self,
        scope_id: &str,
    ) -> std::result::Result<Vec<Value>, String> {
        Ok(self
            .scoped_plugin_surface(scope_id)?
            .capabilities
            .into_iter()
            .map(bridge_tool)
            .collect())
    }

    pub(crate) fn scoped_tool_owner(
        &self,
        scope_id: &str,
        name: &str,
        requested_plugin: Option<&str>,
    ) -> std::result::Result<Option<(String, String)>, String> {
        Ok(self
            .scoped_plugin_surface(scope_id)?
            .capabilities
            .into_iter()
            .find(|capability| {
                requested_plugin.is_none_or(|plugin| plugin == capability.plugin_id)
                    && (capability_tool_name(capability) == name
                        || capability.capability_id == name)
                    && capability
                        .availability
                        .as_deref()
                        .is_none_or(|state| state == "ready")
            })
            .map(|capability| (capability.plugin_id, capability.capability_id)))
    }

    fn scoped_plugin_surface(&self, scope_id: &str) -> std::result::Result<PluginSurface, String> {
        if scope_id.is_empty()
            || !scope_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(format!(
                "invalid scope_id {scope_id:?}; expected non-empty ASCII identifier"
            ));
        }
        let surface = self.get_json(&format!(
            "/api/mcp/plugins/scoped-surface?scope_id={scope_id}"
        ))?;
        let label = surface.to_string();
        let surface = serde_json::from_value::<PluginSurface>(surface).map_err(|error| {
            format!(
                "invalid scoped app plugin surface {label}; expected versioned catalog: {error}"
            )
        })?;
        Ok(surface)
    }

    fn capability_for_tool(
        &self,
        tool_name: &str,
    ) -> std::result::Result<PluginCapability, String> {
        let surface = match self.cached_surface()? {
            Some(surface) => surface,
            None => self.refresh_surface()?,
        };
        surface
            .capabilities
            .into_iter()
            .find(|capability| capability_tool_name(capability) == tool_name)
            .ok_or_else(|| {
                format!("missing app tool {tool_name:?}; expected advertised capability")
            })
    }

    fn refresh_surface(&self) -> std::result::Result<PluginSurface, String> {
        let value = self.get_json("/api/mcp/plugins/surface")?;
        let label = value.to_string();
        let surface = serde_json::from_value::<PluginSurface>(value).map_err(|error| {
            format!("invalid app plugin surface {label}; expected versioned catalog: {error}")
        })?;
        *self
            .last_known_surface
            .lock()
            .map_err(|_| "app bridge catalog cache lock is poisoned".to_string())? =
            Some(surface.clone());
        Ok(surface)
    }

    fn cached_surface(&self) -> std::result::Result<Option<PluginSurface>, String> {
        Ok(self
            .last_known_surface
            .lock()
            .map_err(|_| "app bridge catalog cache lock is poisoned".to_string())?
            .clone())
    }

    fn stale_surface(&self, error: String) -> std::result::Result<Value, String> {
        let Some(mut surface) = self.cached_surface()? else {
            return Err(error);
        };
        surface.freshness = "stale".into();
        let mut value = serde_json::to_value(surface).map_err(|error| error.to_string())?;
        value["catalog_error"] = Value::String(error);
        Ok(value)
    }

    fn invoke_controlled(
        &self,
        context: &McpInvocationContext,
        capability: &PluginCapability,
        input: Value,
    ) -> std::result::Result<AppBridgeInvocationResponseV1, String> {
        self.invoke_controlled_with_identity(context, context, capability, input, false)
    }

    fn invoke_controlled_with_identity(
        &self,
        active_context: &McpInvocationContext,
        wire_context: &McpInvocationContext,
        capability: &PluginCapability,
        input: Value,
        interrupt_on_cancel: bool,
    ) -> std::result::Result<AppBridgeInvocationResponseV1, String> {
        let cancellation = active_context.cancellation();
        let connection = self.config.current_connection_until(
            std::time::Instant::now() + wire_context.remaining(),
            cancellation.as_atomic(),
        )?;
        let request = AppBridgeInvocationRequestV1 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id: wire_context.request_id().to_owned(),
            owner_id: wire_context.owner_id().to_owned(),
            session_id: wire_context.session_id().to_owned(),
            scope_id: wire_context.scope_id().map(str::to_owned),
            deadline_unix_ms: wire_context.deadline_unix_ms(),
            plugin_id: capability.plugin_id.clone(),
            capability_id: capability.capability_id.clone(),
            input_json: serde_json::to_vec(&input).map_err(|error| error.to_string())?,
        };
        self.register_target(
            active_context,
            capability,
            wire_context,
            connection.clone(),
            interrupt_on_cancel,
        )?;
        let result = self.post_protobuf_with_timeout(
            &connection,
            "/api/mcp/plugins/invoke-v1",
            request.encode_to_vec(),
            active_context.remaining(),
        );
        self.remove_target(active_context);
        let response = result?;
        let response = AppBridgeInvocationResponseV1::decode(response.as_slice())
            .map_err(|error| format!("invalid app bridge invocation response: {error}"))?;
        require_protocol(response.protocol_major)?;
        Ok(response)
    }

    fn register_target(
        &self,
        active_context: &McpInvocationContext,
        capability: &PluginCapability,
        wire_context: &McpInvocationContext,
        connection: RuntimeConnection,
        interrupt_on_cancel: bool,
    ) -> std::result::Result<(), String> {
        self.active_targets
            .lock()
            .map_err(|_| "app bridge active target lock is poisoned".to_string())?
            .insert(
                context_key(active_context),
                ActiveTarget {
                    plugin_id: capability.plugin_id.clone(),
                    connection,
                    session_id: wire_context.session_id().to_owned(),
                    scope_id: wire_context.scope_id().map(str::to_owned),
                    interrupt_on_cancel,
                },
            );
        Ok(())
    }

    fn remove_target(&self, context: &McpInvocationContext) {
        if let Ok(mut targets) = self.active_targets.lock() {
            targets.remove(&context_key(context));
        }
    }

    fn active_target(
        &self,
        context: &McpInvocationContext,
    ) -> std::result::Result<Option<ActiveTarget>, String> {
        Ok(self
            .active_targets
            .lock()
            .map_err(|_| "app bridge active target lock is poisoned".to_string())?
            .get(&context_key(context))
            .cloned())
    }

    fn get_json(&self, path: &str) -> std::result::Result<Value, String> {
        let connection = self.discovery_connection()?;
        self.request_json_safe(&connection, AppBridgeHttpMethod::Get, path, Value::Null)
    }

    fn discovery_connection(&self) -> Result<RuntimeConnection, String> {
        self.startup.probe()?.ok_or_else(|| {
            "app is initializing; expected a ready app bridge before plugin discovery".into()
        })
    }

    fn post_protobuf(
        &self,
        connection: &RuntimeConnection,
        path: &str,
        body: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, String> {
        let path = credentialed_path(connection, path);
        self.transport
            .post_protobuf(&connection.app_bridge_base_url, &path, &body)
            .map_err(|error| error.to_string())
    }

    fn post_protobuf_with_timeout(
        &self,
        connection: &RuntimeConnection,
        path: &str,
        body: Vec<u8>,
        timeout: std::time::Duration,
    ) -> std::result::Result<Vec<u8>, String> {
        let path = credentialed_path(connection, path);
        self.transport
            .post_protobuf_with_timeout(&connection.app_bridge_base_url, &path, &body, timeout)
            .map_err(|error| error.to_string())
    }

    fn request_json_safe(
        &self,
        connection: &RuntimeConnection,
        method: AppBridgeHttpMethod,
        path: &str,
        body: Value,
    ) -> std::result::Result<Value, String> {
        let request_path = credentialed_path(connection, path);
        match self.transport.request_json(
            &connection.app_bridge_base_url,
            method,
            &request_path,
            &body,
        ) {
            Ok(value) => Ok(value),
            Err(error @ AppBridgeRequestError::HttpStatus { .. }) => Err(error.to_string()),
            Err(first_error @ AppBridgeRequestError::Transport(_)) => {
                let replacement = self.discovery_connection()?;
                if replacement.generation_nonce == connection.generation_nonce {
                    return Err(first_error.to_string());
                }
                let replacement_path = credentialed_path(&replacement, path);
                self.transport
                    .request_json(
                        &replacement.app_bridge_base_url,
                        method,
                        &replacement_path,
                        &body,
                    )
                    .map_err(|error| error.to_string())
            }
        }
    }
}

fn credentialed_path(connection: &RuntimeConnection, path: &str) -> String {
    format!(
        "{path}{}credential={}",
        if path.contains('?') { '&' } else { '?' },
        connection.app_bridge_credential
    )
}

fn bridge_tool(capability: PluginCapability) -> Value {
    let tool_name = capability_tool_name(&capability);
    let input_schema = capability
        .input_schema
        .unwrap_or_else(|| json!({ "type": "object" }));
    json!({
        "name": tool_name,
        "description": capability.description.unwrap_or_else(|| {
            format!("Invoke app plugin capability {}.", capability.capability_id)
        }),
        "inputSchema": input_schema
    })
}

fn capability_tool_name(capability: &PluginCapability) -> String {
    capability.dynamic_tool_name.clone().unwrap_or_else(|| {
        format!(
            "app_plugin.{}.{}",
            capability.plugin_id, capability.capability_id
        )
    })
}

fn invocation_input(arguments: Value) -> Value {
    arguments.get("input").cloned().unwrap_or(arguments)
}

fn direct_capability(arguments: Value) -> std::result::Result<(PluginCapability, Value), String> {
    let plugin_id = required_string(&arguments, "plugin_id")?;
    let capability_id = required_string(&arguments, "capability_id")?;
    let input = arguments.get("input").cloned().unwrap_or(Value::Null);
    Ok((
        PluginCapability {
            plugin_id,
            capability_id,
            dynamic_tool_name: None,
            input_schema: None,
            availability: None,
            description: None,
        },
        input,
    ))
}

fn required_string(arguments: &Value, field: &str) -> std::result::Result<String, String> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            format!("missing {field} in {arguments}; expected non-empty app bridge identity")
        })
}

fn require_protocol(protocol_major: u32) -> std::result::Result<(), String> {
    if protocol_major == APP_BRIDGE_PROTOCOL_MAJOR {
        return Ok(());
    }
    Err(format!(
        "app bridge protocol major {protocol_major}; expected {APP_BRIDGE_PROTOCOL_MAJOR}"
    ))
}

fn context_key(context: &McpInvocationContext) -> (String, String) {
    (
        context.owner_id().to_owned(),
        context.request_id().to_owned(),
    )
}

fn coordinator_for(root: PathBuf) -> AppRuntimeCoordinator {
    let launch_root = root.clone();
    AppRuntimeCoordinator::new(root, move |request: lumvise_app_core::ActivationRequest| {
        let executable = std::env::current_exe().map_err(|error| {
            lumvise_app_core::RuntimeCoordinatorError::Control(error.to_string())
        })?;
        std::process::Command::new(executable)
            .args(
                request
                    .background
                    .then_some(crate::product::BACKGROUND_LAUNCH_FLAG),
            )
            .env("LUMVISE_RUNTIME_ROOT", &launch_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(lumvise_app_core::RuntimeCoordinatorError::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::thread;
    #[test]
    fn capability_tool_name_uses_canonical_name_when_surface_omits_dynamic_name() {
        let capability = PluginCapability {
            plugin_id: "plugin.example".to_string(),
            capability_id: "run".to_string(),
            dynamic_tool_name: None,
            description: None,
            input_schema: None,
            availability: None,
        };
        assert_eq!(
            capability_tool_name(&capability),
            "app_plugin.plugin.example.run"
        );
    }

    #[test]
    fn cloned_config_observes_terminal_owner_shutdown_without_relaunch() {
        let workspace = tempfile::tempdir().expect("runtime workspace");
        let launcher_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let launcher_calls_for_coordinator = Arc::clone(&launcher_calls);
        let coordinator = Arc::new(AppRuntimeCoordinator::new(workspace.path(), move |_| {
            launcher_calls_for_coordinator.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        let config = AppBridgeConfig {
            discovery_path: workspace.path().join(RUNTIME_DISCOVERY_FILE),
            coordinator,
            terminal: Arc::new(AtomicBool::new(false)),
        };
        let clone = config.clone();
        let mut owner = match config
            .coordinator()
            .acquire_or_forward(lumvise_app_core::ActivationRequest::default())
            .expect("owner lease")
        {
            lumvise_app_core::AcquireResult::Owner(owner) => owner,
            lumvise_app_core::AcquireResult::Forwarded(_) => panic!("expected owner"),
        };
        let generation = owner
            .mark_ready("http://127.0.0.1:61238")
            .expect("ready generation");
        let observed = clone
            .current_connection_until(
                std::time::Instant::now() + std::time::Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .expect("clone observes generation");
        assert_eq!(observed.generation_nonce, generation.generation_nonce);
        owner
            .begin_quit(lumvise_app_core::QuitRequest::default())
            .expect("owner quit");

        let terminal = clone.current_connection_until(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            &AtomicBool::new(false),
        );
        assert!(
            terminal
                .expect_err("terminal coordinator must not relaunch")
                .contains("runtime is quitting")
        );
        assert!(clone.is_terminal());
        assert!(config.is_terminal());
        assert_eq!(launcher_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn discovery_uses_coordinator_canonical_root() {
        let config = AppBridgeConfig::discovery();
        let production_root = AppRuntimeCoordinator::production()
            .runtime_root()
            .to_path_buf();
        assert_eq!(config.runtime_root(), production_root);
        assert_eq!(
            config.runtime_root(),
            config.coordinator().runtime_root().to_path_buf()
        );
    }

    #[test]
    fn custom_root_rebinds_discovery_and_coordinator_together() {
        let config = AppBridgeConfig::discovery().with_runtime_root("/tmp/lumvise-adapter-test");
        assert_eq!(
            config.runtime_root(),
            PathBuf::from("/tmp/lumvise-adapter-test")
        );
        assert_eq!(
            config.coordinator().runtime_root(),
            Path::new("/tmp/lumvise-adapter-test")
        );
    }
    #[test]
    fn cancellation_uses_logical_scoped_target_then_interrupts_and_cleans_up() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake bridge");
        let base_url = format!("http://{}", listener.local_addr().expect("fake address"));
        let (requests_sender, requests_receiver) = channel();
        let server = thread::spawn(move || {
            for response in [
                AppBridgeCancellationResponseV1 {
                    protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
                    cancelled: true,
                }
                .encode_to_vec(),
                AppBridgeInvocationResponseV1 {
                    protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
                    request_id: "interrupted".into(),
                    status: AppBridgeInvocationStatusV1::Completed as i32,
                    output_json: br#"{}"#.to_vec(),
                    message: String::new(),
                    retryable: false,
                }
                .encode_to_vec(),
            ] {
                let (mut stream, _) = listener.accept().expect("fake bridge request");
                requests_sender
                    .send(read_test_request(&stream))
                    .expect("record fake request");
                write_test_protobuf_response(&mut stream, response);
            }
        });
        let workspace = tempfile::tempdir().expect("workspace");
        let runtime = AppBridgeRuntime::start(
            AppBridgeConfig::discovery().with_runtime_root(workspace.path()),
        );
        let context = McpInvocationContext::with_timeout(
            "parent-request",
            "parent-owner",
            "transport-session",
            None,
            std::time::Duration::from_secs(10),
        );
        runtime
            .active_targets
            .lock()
            .expect("active target lock")
            .insert(
                context_key(&context),
                ActiveTarget {
                    plugin_id: "builtin.assistant".into(),
                    connection: RuntimeConnection {
                        generation_nonce: "generation".into(),
                        app_bridge_credential: "credential".into(),
                        app_bridge_credential_expires_unix_ms: u64::MAX,
                        app_bridge_base_url: base_url,
                    },
                    session_id: "logical-session".into(),
                    scope_id: Some("assistant_session".into()),
                    interrupt_on_cancel: true,
                },
            );

        assert!(runtime.cancel_controlled(&context).expect("cancelled"));
        let requests = [
            requests_receiver.recv().unwrap(),
            requests_receiver.recv().unwrap(),
        ];
        server.join().expect("fake bridge exits");
        let cancellation = AppBridgeCancellationRequestV1::decode(requests[0].1.as_slice())
            .expect("decode cancellation");
        let interrupted = AppBridgeInvocationRequestV1::decode(requests[1].1.as_slice())
            .expect("decode terminal transition");

        assert!(
            requests[0]
                .0
                .starts_with("POST /api/mcp/plugins/cancel-v1?credential=")
        );
        assert_eq!(cancellation.request_id, "parent-request");
        assert_eq!(cancellation.owner_id, "parent-owner");
        assert_eq!(cancellation.session_id, "logical-session");
        assert_eq!(cancellation.scope_id.as_deref(), Some("assistant_session"));
        assert_eq!(cancellation.plugin_id, "builtin.assistant");
        assert!(
            requests[1]
                .0
                .starts_with("POST /api/mcp/plugins/invoke-v1?credential=")
        );
        assert!(
            interrupted
                .request_id
                .starts_with("parent-request:interrupted:")
        );
        assert_eq!(interrupted.owner_id, "parent-owner");
        assert_eq!(interrupted.session_id, "logical-session");
        assert_eq!(interrupted.scope_id, None);
        assert_eq!(interrupted.plugin_id, "builtin.assistant");
        assert_eq!(interrupted.capability_id, "advance_assistant_state");
        assert_eq!(
            serde_json::from_slice::<Value>(&interrupted.input_json).unwrap()["event"],
            "interrupted"
        );
        assert!(runtime.active_target(&context).unwrap().is_none());
    }

    fn read_test_request(stream: &TcpStream) -> (String, Vec<u8>) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone request stream"));
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read request line");
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read request header");
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().expect("content length");
                }
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).expect("read request body");
        (request_line.trim_end().into(), body)
    }

    fn write_test_protobuf_response(stream: &mut TcpStream, body: Vec<u8>) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/protobuf\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .expect("write response headers");
        stream.write_all(&body).expect("write response body");
    }
}
