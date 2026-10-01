use crate::AppCore;
use crate::PluginMcpTool;
use crate::app::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use lumvise_mcp_core::{
    APP_BRIDGE_PROTOCOL_MAJOR, AppBridgeCancellationRequestV1, AppBridgeCancellationResponseV1,
    AppBridgeInvocationRequestV1, AppBridgeInvocationResponseV1, AppBridgeInvocationStatusV1,
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
        ("POST", "/api/mcp/plugins/invoke-v1") => Some(controlled_invoke_response(app, request)),
        ("POST", "/api/mcp/plugins/cancel-v1") => Some(controlled_cancel_response(app, request)),
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
    let payload = AppBridgeInvocationRequestV1::decode(request.body.as_slice())
        .map_err(|error| error_response("400 Bad Request", error))?;
    validate_controlled_request(&payload)
        .map_err(|error| error_response("400 Bad Request", error))?;
    let request_id = payload.request_id.clone();
    let runtime_request = controlled_runtime_request(app, &payload).map_err(|message| {
        protobuf_response(bridge_failure(
            request_id.clone(),
            AppBridgeInvocationStatusV1::Internal,
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
    payload: &AppBridgeInvocationRequestV1,
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
    let context = RuntimeInvocationContext::new(
        &payload.request_id,
        &payload.owner_id,
        RuntimeInvocationClass::Foreground,
        monotonic_deadline(payload.deadline_unix_ms),
    )
    .with_route_identity(&payload.session_id, payload.scope_id.clone());
    Ok(RuntimeInvocationRequest::new(
        plugin_id, export_id, input, context,
    ))
}

fn global_mcp_route(
    app: &AppCore,
    payload: &AppBridgeInvocationRequestV1,
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
    let payload = match AppBridgeCancellationRequestV1::decode(request.body.as_slice()) {
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
    protobuf_response(AppBridgeCancellationResponseV1 {
        protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
        cancelled,
    })
}

fn validate_controlled_request(payload: &AppBridgeInvocationRequestV1) -> Result<(), String> {
    if payload.protocol_major != APP_BRIDGE_PROTOCOL_MAJOR {
        return Err(format!(
            "app bridge protocol major {}; expected {APP_BRIDGE_PROTOCOL_MAJOR}",
            payload.protocol_major
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

fn bridge_wire_outcome(request_id: String, outcome: WireOutcome) -> AppBridgeInvocationResponseV1 {
    match outcome {
        WireOutcome::Succeeded { value } => AppBridgeInvocationResponseV1 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id,
            status: AppBridgeInvocationStatusV1::Completed as i32,
            output_json: serde_json::to_vec(&value).unwrap_or_default(),
            message: String::new(),
            retryable: false,
        },
        WireOutcome::Failed { error } => bridge_failure(
            request_id,
            AppBridgeInvocationStatusV1::Failed,
            error.message,
            error.retryable,
        ),
    }
}

fn bridge_runtime_error(
    request_id: String,
    error: RuntimeInvocationError,
) -> AppBridgeInvocationResponseV1 {
    let status = match error.kind() {
        RuntimeFailureKind::Busy => AppBridgeInvocationStatusV1::Busy,
        RuntimeFailureKind::DeadlineExceeded => AppBridgeInvocationStatusV1::DeadlineExceeded,
        RuntimeFailureKind::Cancelled => AppBridgeInvocationStatusV1::Cancelled,
        RuntimeFailureKind::Unavailable => AppBridgeInvocationStatusV1::Unavailable,
        RuntimeFailureKind::PluginFailure => AppBridgeInvocationStatusV1::Failed,
        RuntimeFailureKind::InvalidInput | RuntimeFailureKind::Internal => {
            AppBridgeInvocationStatusV1::Internal
        }
    };
    bridge_failure(request_id, status, error.to_string(), error.retryable())
}

fn bridge_failure(
    request_id: String,
    status: AppBridgeInvocationStatusV1,
    message: String,
    retryable: bool,
) -> AppBridgeInvocationResponseV1 {
    AppBridgeInvocationResponseV1 {
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
