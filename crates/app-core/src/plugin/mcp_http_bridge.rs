use crate::AppCore;
use crate::PluginMcpTool;
use crate::app::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use lumvise_mcp_core::{
    APP_BRIDGE_PROTOCOL_MAJOR, AppBridgeCancellationRequestV2, AppBridgeCancellationResponseV2,
    AppBridgeInvocationRequestV2, AppBridgeInvocationResponseV2, AppBridgeInvocationStatusV2,
};
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    PluginInvocationCancellationRequest as RuntimeCancellationRequest,
    PluginInvocationClass as RuntimeInvocationClass,
    PluginInvocationContext as RuntimeInvocationContext,
    PluginInvocationError as RuntimeInvocationError,
    PluginInvocationFailureKind as RuntimeFailureKind, PluginInvocationHandle,
    PluginInvocationRequest as RuntimeInvocationRequest,
};
use prost::Message;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{mcp_catalog::McpCatalog, scoped_mcp::scoped_mcp_route};

/// Every path under this prefix is App Bridge territory and requires the
/// runtime bridge credential; everything else bypasses the bridge entirely.
const BRIDGE_ROUTE_PREFIX: &str = "/api/mcp/plugins/";

pub(crate) fn plugin_mcp_bridge_response(
    app: &AppCore,
    request: &HttpRequest,
) -> Option<HttpResponse> {
    // The credential wall guards only the App Bridge MCP routes. Every other
    // path falls through to the plugin dispatcher uncredentialed: renderer
    // iframes load signed View assets without a bridge credential, and
    // compiled plugin HTTP routes perform no credential check of their own.
    if !request.path.starts_with(BRIDGE_ROUTE_PREFIX) {
        return None;
    }
    if is_authenticated_bridge_request(app, request).is_err() {
        return Some(error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        ));
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/mcp/plugins/surface") => Some(plugin_surface_response(app)),
        ("GET", "/api/mcp/plugins/scoped-surface") => {
            Some(scoped_plugin_surface_response(app, request))
        }
        ("POST", "/api/mcp/plugins/invoke-v2") => Some(controlled_invoke_response(app, request)),
        ("POST", "/api/mcp/plugins/cancel-v2") => Some(controlled_cancel_response(app, request)),
        _ => None,
    }
}
pub(crate) fn is_authenticated_bridge_request(
    app: &AppCore,
    request: &HttpRequest,
) -> Result<(), &'static str> {
    let query_credential = request
        .query
        .get("credential")
        .filter(|value| !value.trim().is_empty())
        .map(String::as_str);
    let header_credential = request
        .authorization
        .as_deref()
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim());
    let credential = match (query_credential, header_credential) {
        (Some(query), Some(header)) if query != header => return Err("credential mismatch"),
        (Some(query), _) => query,
        (_, Some(header)) => header,
        (None, None) => return Err("missing credential"),
    };
    if app.bridge_credential_matches(credential) {
        Ok(())
    } else {
        Err("credential mismatch")
    }
}

fn controlled_invoke_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let (request_id, handle) = match start_controlled_invoke(app, request) {
        Ok(started) => started,
        Err(response) => return response,
    };
    controlled_invoke_result(request_id, handle.blocking_wait())
}

pub(crate) fn start_controlled_invoke<'app>(
    app: &'app AppCore,
    request: &HttpRequest,
) -> Result<(String, PluginInvocationHandle<'app>), HttpResponse> {
    let payload = AppBridgeInvocationRequestV2::decode(request.body.as_slice())
        .map_err(|error| error_response("400 Bad Request", error))?;
    validate_controlled_request(&payload)
        .map_err(|error| error_response("400 Bad Request", error))?;
    let request_id = payload.request_id.clone();
    let runtime_request = controlled_runtime_request(app, &payload).map_err(|message| {
        protobuf_response(bridge_failure(
            request_id.clone(),
            AppBridgeInvocationStatusV2::Internal,
            message,
            false,
        ))
    })?;
    let handle = app
        .plugin_system()
        .start_controlled_invocation(runtime_request)
        .map_err(|error| protobuf_response(bridge_runtime_error(request_id.clone(), error)))?;
    Ok((request_id, handle))
}

pub(crate) fn controlled_invoke_result(
    request_id: String,
    result: Result<WireOutcome, RuntimeInvocationError>,
) -> HttpResponse {
    let response = match result {
        Ok(outcome) => bridge_wire_outcome(request_id, outcome),
        Err(error) => bridge_runtime_error(request_id, error),
    };
    protobuf_response(response)
}

fn controlled_runtime_request(
    app: &AppCore,
    payload: &AppBridgeInvocationRequestV2,
) -> Result<RuntimeInvocationRequest, String> {
    let (plugin_id, export_id) = match payload.scope_id.as_deref() {
        Some(scope_id) => scoped_mcp_route(
            &app.plugin_endpoints(),
            scope_id,
            &payload.plugin_id,
            &payload.capability_id,
        )
        .or_else(|_| global_mcp_route(app, payload))
        .map_err(|error| error.to_string())?,
        None => global_mcp_route(app, payload).map_err(|error| error.to_string())?,
    };
    let input = serde_json::from_slice(&payload.input_json).map_err(|error| error.to_string())?;
    let context = bridge_invocation_context(payload);
    let context = capture_mcp_project(app, context)?;
    Ok(RuntimeInvocationRequest::new(
        plugin_id, export_id, input, context,
    ))
}

fn bridge_invocation_context(payload: &AppBridgeInvocationRequestV2) -> RuntimeInvocationContext {
    RuntimeInvocationContext::new(
        &payload.request_id,
        &payload.owner_id,
        RuntimeInvocationClass::Foreground,
        monotonic_deadline(payload.deadline_unix_ms),
    )
    .with_route_identity(&payload.session_id, payload.scope_id.clone())
    .with_scope_epoch(payload.scope_epoch)
}

fn capture_mcp_project(
    app: &AppCore,
    context: RuntimeInvocationContext,
) -> Result<RuntimeInvocationContext, String> {
    if context.scope_id() != Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE) {
        return Ok(context);
    }
    let connection = context.session_id().unwrap_or_default();
    let Some(root) = app.mcp_session_membership.project_root(connection)? else {
        return Ok(context.with_scope_epoch(None));
    };
    let bridge = super::plugin_host_capabilities::invocation_control(&context);
    let project = app
        .semantic
        .execute(
            lumvise_db_core::SemanticOperation::ProjectAccess {
                project_root: root.clone(),
            },
            &bridge.control(),
        )
        .map_err(|error| error.to_string())?;
    let lumvise_db_core::SemanticResult::ProjectAccess(project) = project else {
        return Err(format!(
            "MCP project access {project:?}; expected fresh project membership"
        ));
    };
    let (project_id, authority) = match project {
        lumvise_db_core::SemanticProjectAccess::Local { project_id } => (project_id, None),
        lumvise_db_core::SemanticProjectAccess::Member {
            project_id,
            storage_authority,
            ..
        } => (project_id, Some(storage_authority)),
        lumvise_db_core::SemanticProjectAccess::Denied => {
            return Err(format!(
                "MCP project {root:?}; expected current project membership"
            ));
        }
    };
    let epoch = app.mcp_session_membership.capture_project(
        connection,
        &root,
        &project_id,
        authority.as_deref(),
    )?;
    Ok(context.with_scope_epoch(Some(epoch)))
}

fn global_mcp_route(
    app: &AppCore,
    payload: &AppBridgeInvocationRequestV2,
) -> crate::Result<(String, String)> {
    let tool_name = tool_name(&payload.plugin_id, &payload.capability_id);
    let route = McpCatalog::load(&app.plugin_endpoints())?.route(&tool_name)?;
    if route.plugin_id != payload.plugin_id || route.export_id != payload.capability_id {
        return Err(crate::AppCoreError::invalid_value(
            format!("{}/{}", payload.plugin_id, payload.capability_id),
            "advertised MCP route",
        ));
    }
    Ok((route.plugin_id, route.export_id))
}

fn controlled_cancel_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let payload = match AppBridgeCancellationRequestV2::decode(request.body.as_slice()) {
        Ok(payload) => payload,
        Err(error) => return error_response("400 Bad Request", error),
    };
    if payload.protocol_major != APP_BRIDGE_PROTOCOL_MAJOR {
        return error_response("400 Bad Request", "unsupported app bridge protocol major");
    }
    let cancelled = app
        .plugin_system()
        .cancel_controlled(&RuntimeCancellationRequest {
            plugin_id: Some(payload.plugin_id),
            request_id: payload.request_id,
            owner_id: payload.owner_id,
            session_id: Some(payload.session_id),
            scope_id: payload.scope_id,
        })
        .unwrap_or(false);
    protobuf_response(AppBridgeCancellationResponseV2 {
        protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
        cancelled,
    })
}

fn validate_controlled_request(payload: &AppBridgeInvocationRequestV2) -> Result<(), String> {
    if payload.protocol_major != APP_BRIDGE_PROTOCOL_MAJOR {
        return Err(format!(
            "app bridge protocol major {}; expected {APP_BRIDGE_PROTOCOL_MAJOR}",
            payload.protocol_major
        ));
    }
    if payload.scope_id.as_deref() == Some("assistant_session")
        && (payload.owner_id != "builtin.assistant"
            || !payload.scope_epoch.is_some_and(|epoch| epoch > 0))
    {
        return Err(format!(
            "Assistant bridge identity {:?}/{:?}; expected builtin.assistant with positive captured epoch",
            payload.owner_id, payload.scope_epoch
        ));
    }
    for (field, value) in [
        ("request_id", &payload.request_id),
        ("owner_id", &payload.owner_id),
        ("session_id", &payload.session_id),
        ("plugin_id", &payload.plugin_id),
        ("capability_id", &payload.capability_id),
    ] {
        if value.trim().is_empty() {
            return Err(format!(
                "app bridge field {field} was empty; expected identity"
            ));
        }
    }
    Ok(())
}

fn monotonic_deadline(deadline_unix_ms: u64) -> Instant {
    let deadline = UNIX_EPOCH + Duration::from_millis(deadline_unix_ms);
    let remaining = deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    Instant::now()
        .checked_add(remaining)
        .unwrap_or_else(Instant::now)
}

fn bridge_wire_outcome(request_id: String, outcome: WireOutcome) -> AppBridgeInvocationResponseV2 {
    match outcome {
        WireOutcome::Succeeded { value } => AppBridgeInvocationResponseV2 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id,
            status: AppBridgeInvocationStatusV2::Completed as i32,
            output_json: serde_json::to_vec(&value).unwrap_or_default(),
            message: String::new(),
            retryable: false,
        },
        WireOutcome::Failed { error } => bridge_failure(
            request_id,
            AppBridgeInvocationStatusV2::Failed,
            error.message,
            error.retryable,
        ),
    }
}

fn bridge_runtime_error(
    request_id: String,
    error: RuntimeInvocationError,
) -> AppBridgeInvocationResponseV2 {
    let status = match error.kind() {
        RuntimeFailureKind::Busy => AppBridgeInvocationStatusV2::Busy,
        RuntimeFailureKind::DeadlineExceeded => AppBridgeInvocationStatusV2::DeadlineExceeded,
        RuntimeFailureKind::Cancelled => AppBridgeInvocationStatusV2::Cancelled,
        RuntimeFailureKind::Unavailable => AppBridgeInvocationStatusV2::Unavailable,
        RuntimeFailureKind::PluginFailure => AppBridgeInvocationStatusV2::Failed,
        RuntimeFailureKind::InvalidInput | RuntimeFailureKind::Internal => {
            AppBridgeInvocationStatusV2::Internal
        }
    };
    bridge_failure(request_id, status, error.to_string(), error.retryable())
}

fn bridge_failure(
    request_id: String,
    status: AppBridgeInvocationStatusV2,
    message: String,
    retryable: bool,
) -> AppBridgeInvocationResponseV2 {
    AppBridgeInvocationResponseV2 {
        protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
        request_id,
        status: status as i32,
        output_json: Vec::new(),
        message,
        retryable,
    }
}

fn protobuf_response(message: impl Message) -> HttpResponse {
    HttpResponse::buffered(
        "200 OK",
        "application/protobuf",
        message.encode_to_vec(),
        Vec::new(),
    )
}

fn plugin_surface_response(app: &AppCore) -> HttpResponse {
    let tools = match app.plugin_endpoints().plugin_mcp_tools() {
        Ok(tools) => tools,
        Err(error) => return error_response("500 Internal Server Error", error),
    };
    let capabilities = tools
        .into_iter()
        .map(|tool| plugin_capability(app, tool))
        .collect::<Vec<_>>();
    let generation = surface_generation(&capabilities);
    json_response(
        "200 OK",
        json!({
            "generation": generation,
            "freshness": "current",
            "capabilities": capabilities,
        }),
    )
}
fn scoped_plugin_surface_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let Some(scope_id) = request
        .query
        .get("scope_id")
        .map(String::as_str)
        .filter(|scope_id| !scope_id.trim().is_empty())
    else {
        return error_response("400 Bad Request", "missing scope_id query parameter");
    };
    let tools = match app.plugin_endpoints().scoped_mcp_tools(scope_id) {
        Ok(tools) => tools,
        Err(error) => return error_response("500 Internal Server Error", error),
    };
    let capabilities = tools
        .into_iter()
        .map(|tool| plugin_capability(app, tool))
        .collect::<Vec<_>>();
    json_response(
        "200 OK",
        json!({
            "generation": surface_generation(&capabilities),
            "freshness": "current",
            "capabilities": capabilities,
        }),
    )
}

fn plugin_capability(app: &AppCore, tool: PluginMcpTool) -> Value {
    let capability_id = capability_id(&tool.plugin_id, &tool.tool_name);
    let input_schema = tool.mcp_schema();
    json!({
        "plugin_id": tool.plugin_id,
        "capability_id": capability_id,
        "input_schema": input_schema,
        "description": tool.description,
        "dynamic_tool_name": tool.tool_name,
        "availability": if app.plugin_system().is_active(&tool.plugin_id).unwrap_or(false) {
            "ready"
        } else {
            "unavailable"
        },
    })
}

fn surface_generation(capabilities: &[Value]) -> String {
    let mut hasher = DefaultHasher::new();
    for capability in capabilities {
        capability.to_string().hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn capability_id(plugin_id: &str, tool_name: &str) -> String {
    tool_name
        .strip_prefix(&format!("app_plugin.{plugin_id}."))
        .unwrap_or(tool_name)
        .to_string()
}

fn tool_name(plugin_id: &str, capability_id: &str) -> String {
    if capability_id.starts_with("app_plugin.") {
        return capability_id.to_string();
    }
    format!("app_plugin.{plugin_id}.{capability_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::http_dispatch::plugin_http_response;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    fn assistant_invocation() -> AppBridgeInvocationRequestV2 {
        AppBridgeInvocationRequestV2 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id: "proof".into(),
            owner_id: "builtin.assistant".into(),
            session_id: "conversation".into(),
            scope_id: Some("assistant_session".into()),
            scope_epoch: Some(27),
            deadline_unix_ms: 1,
            plugin_id: "builtin.knowledge".into(),
            capability_id: "document.space".into(),
            input_json: b"{}".to_vec(),
        }
    }

    #[test]
    fn bridge_rejects_old_major_and_unbound_assistant_identity() {
        let mut payload = assistant_invocation();
        assert!(validate_controlled_request(&payload).is_ok());
        payload.protocol_major = 1;
        assert!(
            validate_controlled_request(&payload)
                .unwrap_err()
                .contains("expected 2")
        );
        payload.protocol_major = APP_BRIDGE_PROTOCOL_MAJOR;
        payload.scope_epoch = None;
        assert!(validate_controlled_request(&payload).is_err());
        payload.scope_epoch = Some(27);
        payload.owner_id = "model-supplied".into();
        assert!(validate_controlled_request(&payload).is_err());
    }

    #[test]
    fn bridge_keeps_captured_epoch_and_expired_deadline() {
        let context = bridge_invocation_context(&assistant_invocation());
        assert_eq!(context.owner_id(), "builtin.assistant");
        assert_eq!(context.scope_epoch(), Some(27));
        assert_eq!(context.session_id(), Some("conversation"));
        assert!(context.deadline() <= Instant::now());
    }

    #[test]
    fn mcp_ingress_discards_supplied_epoch_and_fences_project_switch() {
        let app = AppCore::in_memory().unwrap();
        let mut payload = assistant_invocation();
        payload.scope_id = Some(lumvise_contracts::KNOWLEDGE_MCP_SCOPE.into());
        payload.deadline_unix_ms = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
            + Duration::from_secs(60))
        .as_millis() as u64;
        let context = bridge_invocation_context(&payload);
        assert!(
            capture_mcp_project(&app, context.clone())
                .unwrap()
                .scope_epoch()
                .is_none()
        );
        app.mcp_session_membership
            .register("conversation", "/first")
            .unwrap();
        let captured = capture_mcp_project(&app, context).unwrap();
        let epoch = captured.scope_epoch().unwrap();
        assert_ne!(epoch, 27);
        assert_eq!(
            app.mcp_session_membership
                .scope("conversation", Some(epoch))
                .unwrap()
                .project_root,
            "/first"
        );
        app.mcp_session_membership
            .register("conversation", "/second")
            .unwrap();
        assert!(
            app.mcp_session_membership
                .scope("conversation", Some(epoch))
                .is_err()
        );
    }

    fn request(
        method: &str,
        path: &str,
        authorization: Option<&str>,
        credential: Option<&str>,
    ) -> HttpRequest {
        let mut query = BTreeMap::new();
        if let Some(credential) = credential {
            query.insert("credential".into(), credential.into());
        }
        HttpRequest {
            method: method.into(),
            path: path.into(),
            query,
            authorization: authorization.map(str::to_string),
            body: Vec::new(),
        }
    }

    fn bridge_request(authorization: Option<&str>, credential: Option<&str>) -> HttpRequest {
        request("GET", "/api/mcp/plugins/surface", authorization, credential)
    }
    #[test]
    fn bridge_accepts_bearer_header_and_rejects_malformed_credentials() {
        let app = credentialed_app();

        assert!(
            is_authenticated_bridge_request(&app, &bridge_request(Some("Bearer grant-1"), None))
                .is_ok()
        );
        assert!(
            is_authenticated_bridge_request(&app, &bridge_request(Some("Basic grant-1"), None))
                .is_err()
        );
        assert!(
            is_authenticated_bridge_request(
                &app,
                &bridge_request(Some("Bearer grant-2"), Some("grant-1"))
            )
            .is_err()
        );
    }

    fn credentialed_app() -> AppCore {
        let app = AppCore::in_memory().expect("in-memory app");
        app.install_bridge_credential_store(Arc::new(Mutex::new(std::collections::HashMap::new())))
            .expect("credential store");
        app.set_bridge_credential(Some("grant-1".into()), u64::MAX)
            .expect("credential");
        app
    }

    #[test]
    fn view_asset_requests_bypass_the_bridge_credential_wall() {
        let app = AppCore::in_memory().expect("in-memory app");
        let response = plugin_http_response(
            &app,
            &request(
                "GET",
                "/api/plugin-views/builtin.canvas/board/assets/index.html",
                None,
                None,
            ),
        )
        .expect("view asset path must reach the plugin dispatcher");
        assert_eq!(response.status, "404 Not Found");
        let body = String::from_utf8_lossy(response.buffered_bytes().expect("buffered body"));
        assert!(body.contains("compiled View asset unavailable"));
    }

    #[test]
    fn bridge_routes_still_reject_uncredentialed_requests() {
        let app = AppCore::in_memory().expect("in-memory app");
        let response = plugin_http_response(&app, &bridge_request(None, None))
            .expect("bridge path must stay bridge-owned");
        assert_eq!(response.status, "401 Unauthorized");
        let body = String::from_utf8_lossy(response.buffered_bytes().expect("buffered body"));
        assert!(body.contains("invalid or missing runtime bridge credential"));
    }

    #[test]
    fn credentialed_bridge_requests_still_succeed() {
        let app = credentialed_app();
        let response = plugin_http_response(&app, &bridge_request(Some("Bearer grant-1"), None))
            .expect("credentialed surface request must be served");
        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn unknown_bridge_subpaths_stay_credential_gated() {
        let app = AppCore::in_memory().expect("in-memory app");
        let response = plugin_http_response(
            &app,
            &request("GET", "/api/mcp/plugins/unknown", None, None),
        )
        .expect("bridge prefix must stay bridge-owned");
        assert_eq!(response.status, "401 Unauthorized");
    }
}
