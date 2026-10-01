use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::{
    AppCore, PLUGIN_MCP_MESSAGE_ENDPOINT, PLUGIN_MCP_SSE_ENDPOINT, SCOPED_MCP_MESSAGE_ENDPOINT,
    SCOPED_MCP_SSE_ENDPOINT, plugin::scoped_mcp_rpc::ScopedMcpRouteContext,
};
use serde_json::Value;

/// Returns whether this request targets the app-owned durable storage-change stream.
pub(crate) fn is_storage_change_stream_request(request: &HttpRequest) -> bool {
    request.method == "GET"
        && request.path == super::storage_change_stream::STORAGE_CHANGE_STREAM_ENDPOINT
}

pub(crate) fn route_http_request(app: &AppCore, request: HttpRequest) -> HttpResponse {
    #[cfg(feature = "assistant-e2e")]
    if let Some(response) = super::e2e_control::route_e2e_request(app, &request) {
        return response;
    }
    let request = rewrite_knowledge_routes(request);
    if let Some(response) = super::plugin_settings::plugin_settings_response(app, &request) {
        return response;
    }
    if request.method == "GET" && request.path == "/health" {
        return health_response(app);
    }
    if request.method == "GET" && request.path == "/metrics" {
        return metrics_response();
    }
    if request.method == "GET" && request.path == super::workspace_activity_http::ACTIVITY_ENDPOINT
    {
        return super::workspace_activity_http::activity_response(app, &request);
    }
    if request.method == "GET"
        && request.path == super::storage_change_stream::STORAGE_CHANGES_ENDPOINT
    {
        return super::storage_change_stream::changes_since_response(app, &request);
    }
    if request.method == "GET" && request.path == super::source_file::SOURCE_FILE_ENDPOINT {
        return super::source_file::source_file_response(app, &request);
    }
    if request.path == super::canvas_files::CANVAS_FILE_ENDPOINT
        && matches!(request.method.as_str(), "GET" | "PUT")
    {
        return super::canvas_files::canvas_file_response(app, &request);
    }
    if request.method == "POST" && request.path == super::project_import::PROJECT_IMPORT_ENDPOINT {
        return super::project_import::project_import_response(app, &request);
    }
    // Debug builds hand the local development UI a short-lived bridge
    // credential so it can run in an ordinary browser through the vite dev
    // proxy. Release builds never expose this route: the packaged app routes
    // its requests in-process instead.
    #[cfg(debug_assertions)]
    if request.method == "GET" && request.path == "/api/dev/bridge-credential" {
        return match app.mint_bridge_credential() {
            Ok((credential, expires_unix_ms)) => json_response(
                "200 OK",
                serde_json::json!({
                    "credential": credential,
                    "expires_unix_ms": expires_unix_ms,
                }),
            ),
            Err(error) => error_response("503 Service Unavailable", error.to_string()),
        };
    }
    if let Some(response) =
        super::mcp_session_registry::mcp_session_registry_response(app, &request)
    {
        return response;
    }
    if let Some(response) = super::project_execution_http::project_execution_response(app, &request)
    {
        return response;
    }
    if is_app_owned_mcp_request(&request) {
        return scoped_mcp_response(app, request);
    }
    if let Some(response) = crate::plugin::http_dispatch::plugin_http_response(app, &request) {
        return response;
    }
    scoped_mcp_response(app, request)
}

fn is_app_owned_mcp_request(request: &HttpRequest) -> bool {
    match request.method.as_str() {
        "GET" => {
            request.path == PLUGIN_MCP_SSE_ENDPOINT
                || request.path == PLUGIN_MCP_MESSAGE_ENDPOINT
                || parse_scoped_mcp_path(&request.path, SCOPED_MCP_SSE_ENDPOINT).is_some()
                || parse_scoped_mcp_path(&request.path, SCOPED_MCP_MESSAGE_ENDPOINT).is_some()
        }
        "POST" => {
            request.path == PLUGIN_MCP_SSE_ENDPOINT
                || request.path == PLUGIN_MCP_MESSAGE_ENDPOINT
                || parse_scoped_mcp_path(&request.path, SCOPED_MCP_MESSAGE_ENDPOINT).is_some()
                || parse_scoped_mcp_path(&request.path, SCOPED_MCP_SSE_ENDPOINT).is_some()
        }
        _ => false,
    }
}

fn scoped_mcp_response(app: &AppCore, request: HttpRequest) -> HttpResponse {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", PLUGIN_MCP_SSE_ENDPOINT) => sse_response(PLUGIN_MCP_MESSAGE_ENDPOINT),
        ("GET", PLUGIN_MCP_MESSAGE_ENDPOINT) => streamable_http_get_not_supported(),
        ("POST", PLUGIN_MCP_SSE_ENDPOINT) => plugin_json_rpc_response(app, request.body),
        ("POST", PLUGIN_MCP_MESSAGE_ENDPOINT) => plugin_json_rpc_response(app, request.body),
        ("GET", path) if parse_scoped_mcp_path(path, SCOPED_MCP_MESSAGE_ENDPOINT).is_some() => {
            streamable_http_get_not_supported()
        }
        ("GET", path) => scoped_sse_response(path),
        ("POST", path) => scoped_json_rpc_response(app, path, request.body),
        _ => unknown_app_http_endpoint(),
    }
}

/// The Streamable HTTP MCP transport may probe a message endpoint with `GET`
/// to open an optional server-initiated stream. Lumvise's scoped and global
/// MCP transports only serve request/response `POST` calls at these paths, so
/// per the MCP spec a server that doesn't offer that stream returns `405`, not
/// a bare `404`. Some clients (observed: Claude Code 2.1.205) treat an
/// unmatched `404` here as a hard connection failure and drop every
/// discovered tool instead of continuing without server push.
fn streamable_http_get_not_supported() -> HttpResponse {
    error_response(
        "405 Method Not Allowed",
        "server-initiated streaming is not offered on this endpoint",
    )
}

fn plugin_json_rpc_response(app: &AppCore, body: Vec<u8>) -> HttpResponse {
    let request = match serde_json::from_slice::<Value>(&body) {
        Ok(request) => request,
        Err(error) => return error_response("400 Bad Request", error.to_string()),
    };
    let response = app.plugin_endpoints().handle_plugin_mcp_json_rpc(request);
    match response {
        Ok(Some(response)) => json_response("200 OK", response),
        Ok(None) => HttpResponse::empty("202 Accepted"),
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    }
}

fn scoped_sse_response(path: &str) -> HttpResponse {
    let Some(context) = parse_scoped_mcp_path(path, SCOPED_MCP_SSE_ENDPOINT) else {
        return unknown_app_http_endpoint();
    };
    sse_response(&scoped_message_endpoint(&context))
}

fn scoped_json_rpc_response(app: &AppCore, path: &str, body: Vec<u8>) -> HttpResponse {
    let context = parse_scoped_mcp_path(path, SCOPED_MCP_MESSAGE_ENDPOINT)
        .or_else(|| parse_scoped_mcp_path(path, SCOPED_MCP_SSE_ENDPOINT));
    let Some(context) = context else {
        return unknown_app_http_endpoint();
    };
    json_rpc_response(app, body, context)
}

fn json_rpc_response(app: &AppCore, body: Vec<u8>, context: ScopedMcpRouteContext) -> HttpResponse {
    let request = match serde_json::from_slice::<Value>(&body) {
        Ok(request) => request,
        Err(error) => return error_response("400 Bad Request", error.to_string()),
    };
    let response = app
        .plugin_endpoints()
        .handle_scoped_mcp_json_rpc(request, context);
    match response {
        Ok(Some(response)) => json_response("200 OK", response),
        Ok(None) => HttpResponse::empty("202 Accepted"),
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    }
}

fn sse_response(message_endpoint: &str) -> HttpResponse {
    HttpResponse::buffered(
        "200 OK",
        "text/event-stream",
        format!("event: endpoint\ndata: {message_endpoint}\n\n").into_bytes(),
        Vec::new(),
    )
}

fn parse_scoped_mcp_path(path: &str, prefix: &str) -> Option<ScopedMcpRouteContext> {
    let suffix = path.strip_prefix(prefix)?.strip_prefix('/')?;
    let segments = suffix.split('/').collect::<Vec<_>>();
    if !(3..=4).contains(&segments.len()) || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    let session_epoch = match segments.get(3) {
        Some(epoch) => Some(epoch.parse::<u64>().ok()?),
        None => None,
    };
    Some(ScopedMcpRouteContext {
        scope_id: segments[0].to_string(),
        owner_id: segments[1].to_string(),
        session_id: segments[2].to_string(),
        session_epoch,
    })
}

/// Serves the Prometheus text exposition (`GET /metrics`). Empty when the
/// recorder is disabled (`LUMVISE_METRICS=0`).
fn metrics_response() -> HttpResponse {
    HttpResponse::buffered(
        "200 OK",
        "text/plain; version=0.0.4; charset=utf-8",
        crate::observability::render_metrics().into_bytes(),
        Vec::new(),
    )
}

/// Serves the standard local daemon liveness probe.
///
/// `active_backend` stays `None` until native neural backend reporting is wired.
fn health_response(app: &AppCore) -> HttpResponse {
    use lumvise_contracts::{
        DatabaseState, DatabaseStatus, HealthResponse, NeuralBackend, NeuralRuntimeStatus,
        RuntimeMode,
    };
    let registered_mcp_instances = match app.relational.execute(
        lumvise_db_core::RelationalOperation::ActiveMcpInstances,
        &lumvise_resource_routing::InvocationControl::sixty_seconds(),
    ) {
        Ok(lumvise_db_core::RelationalResult::McpInstances(instances)) => instances.len(),
        _ => 0,
    };
    let health = HealthResponse {
        ok: true,
        app_name: "lumvise".to_string(),
        api_version: lumvise_contracts::API_VERSION.to_string(),
        runtime_mode: RuntimeMode::Daemon,
        database: DatabaseStatus {
            state: DatabaseState::Ready,
            path: "selected relational persistence".to_string(),
        },
        neural: NeuralRuntimeStatus {
            owner: "app-core".to_string(),
            available: true,
            active_backend: NeuralBackend::None,
            loaded_models: Vec::new(),
        },
        registered_mcp_instances,
    };
    match serde_json::to_value(&health) {
        Ok(body) => json_response("200 OK", body),
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    }
}

fn rewrite_knowledge_routes(mut request: HttpRequest) -> HttpRequest {
    request.path = match request.path.as_str() {
        "/api/wiki/manifest" => "/api/knowledge/manifest",
        "/api/wiki/setup" => "/api/knowledge/setup",
        "/api/wiki/export" => "/api/knowledge/export",
        "/api/wiki/page" => "/api/knowledge/page",
        "/api/wiki/write" => "/api/knowledge/write",
        "/api/wiki/resolve-target" => "/api/knowledge/resolve-target",
        "/api/wiki/events" => "/api/knowledge/events",
        _ => &request.path,
    }
    .to_string();
    request
}

fn scoped_message_endpoint(context: &ScopedMcpRouteContext) -> String {
    let path = format!(
        "{}/{}/{}/{}",
        SCOPED_MCP_MESSAGE_ENDPOINT, context.scope_id, context.owner_id, context.session_id
    );
    context
        .session_epoch
        .map_or_else(|| path.clone(), |epoch| format!("{path}/{epoch}"))
}

pub(crate) fn unknown_app_http_endpoint() -> HttpResponse {
    error_response("404 Not Found", "unknown app HTTP endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_endpoint_preserves_epoch_and_rejects_invalid_epoch_paths() {
        let base = format!("{SCOPED_MCP_SSE_ENDPOINT}/scope/owner/session");
        let bound = parse_scoped_mcp_path(&format!("{base}/27"), SCOPED_MCP_SSE_ENDPOINT).unwrap();
        assert_eq!(bound.session_epoch, Some(27));
        assert_eq!(
            scoped_message_endpoint(&bound),
            format!("{SCOPED_MCP_MESSAGE_ENDPOINT}/scope/owner/session/27")
        );
        assert!(
            parse_scoped_mcp_path(&base, SCOPED_MCP_SSE_ENDPOINT)
                .unwrap()
                .session_epoch
                .is_none()
        );
        for suffix in ["-1", "wrong", "18446744073709551616", "27/extra", ""] {
            assert!(
                parse_scoped_mcp_path(&format!("{base}/{suffix}"), SCOPED_MCP_SSE_ENDPOINT)
                    .is_none()
            );
        }
    }

    #[test]
    fn health_route_reports_daemon_liveness() {
        let app = AppCore::in_memory().unwrap();
        let response = route_http_request(
            &app,
            HttpRequest {
                method: "GET".to_string(),
                path: "/health".to_string(),
                query: Default::default(),
                body: Vec::new(),
                authorization: None,
            },
        );
        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(response.buffered_bytes().expect("buffered health body"))
                .unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["api_version"], lumvise_contracts::API_VERSION);
        assert_eq!(body["database"]["state"], "ready");
    }

    #[test]
    fn metrics_route_serves_prometheus_text() {
        let app = AppCore::in_memory().unwrap();
        let response = route_http_request(
            &app,
            HttpRequest {
                method: "GET".to_string(),
                path: "/metrics".to_string(),
                query: Default::default(),
                body: Vec::new(),
                authorization: None,
            },
        );
        assert_eq!(response.status, "200 OK");
        // Prometheus exposition content type, with charset.
        assert!(
            response
                .content_type
                .starts_with("text/plain; version=0.0.4"),
            "metrics content type: {:?}",
            response.content_type
        );
    }

    #[test]
    fn app_owned_mcp_routes_bypass_compiled_surface_catalog() {
        let app = AppCore::in_memory().unwrap();
        crate::plugin::compiled_surfaces::reset_surface_load_count();
        for (method, path, expected_status) in [
            ("GET", PLUGIN_MCP_SSE_ENDPOINT, "200 OK"),
            ("POST", PLUGIN_MCP_MESSAGE_ENDPOINT, "400 Bad Request"),
            (
                "GET",
                "/api/scoped-plugin-mcp/sse/scope-a/owner-a/session-a",
                "200 OK",
            ),
            (
                "POST",
                "/api/scoped-plugin-mcp/messages/scope-a/owner-a/session-a",
                "400 Bad Request",
            ),
        ] {
            let response = route_http_request(
                &app,
                HttpRequest {
                    method: method.to_string(),
                    path: path.to_string(),
                    query: Default::default(),
                    body: b"not-json".to_vec(),
                    authorization: None,
                },
            );
            assert_eq!(
                crate::plugin::compiled_surfaces::surface_load_count(),
                0,
                "{method} {path} unexpectedly loaded generic surfaces"
            );
            assert_eq!(response.status, expected_status, "{method} {path}");
        }
        assert_eq!(
            crate::plugin::compiled_surfaces::surface_load_count(),
            0,
            "app-owned MCP routes must not construct generic compiled surfaces"
        );
    }

    #[test]
    fn get_on_message_only_mcp_endpoints_returns_405_not_404() {
        // The MCP "Streamable HTTP" transport may probe a message endpoint
        // with GET to open an optional server push stream. A bare 404 here
        // (rather than the spec's 405) made Claude Code's MCP client treat
        // the whole server as unreachable and drop every discovered tool
        // (see anthropics/claude-code MCP client behavior, observed 2.1.205).
        let app = AppCore::in_memory().unwrap();
        for path in [
            PLUGIN_MCP_MESSAGE_ENDPOINT.to_string(),
            "/api/scoped-plugin-mcp/messages/scope-a/owner-a/session-a".to_string(),
        ] {
            let response = route_http_request(
                &app,
                HttpRequest {
                    method: "GET".to_string(),
                    path: path.clone(),
                    query: Default::default(),
                    body: Vec::new(),
                    authorization: None,
                },
            );
            assert_eq!(response.status, "405 Method Not Allowed", "GET {path}");
        }

        // The paired SSE endpoint still serves its GET-based handshake.
        let response = route_http_request(
            &app,
            HttpRequest {
                method: "GET".to_string(),
                path: "/api/scoped-plugin-mcp/sse/scope-a/owner-a/session-a".to_string(),
                query: Default::default(),
                body: Vec::new(),
                authorization: None,
            },
        );
        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn storage_change_stream_route_is_exact_get_only() {
        for (method, path, expected) in [
            (
                "GET",
                super::super::storage_change_stream::STORAGE_CHANGE_STREAM_ENDPOINT,
                true,
            ),
            (
                "POST",
                super::super::storage_change_stream::STORAGE_CHANGE_STREAM_ENDPOINT,
                false,
            ),
            ("GET", "/api/storage/changes/events/extra", false),
        ] {
            assert_eq!(
                is_storage_change_stream_request(&HttpRequest {
                    method: method.to_string(),
                    path: path.to_string(),
                    query: Default::default(),
                    body: Vec::new(),
                    authorization: None,
                }),
                expected
            );
        }
    }

    #[test]
    fn removed_compatibility_routes_are_not_dispatchable() {
        assert_routes_unavailable(&[
            ("POST", "/api/semantic-index/batches"),
            ("GET", "/api/plugins/mcp/surface"),
            ("POST", "/api/plugins/mcp/invoke"),
        ]);
    }

    fn assert_routes_unavailable(routes: &[(&str, &str)]) {
        let app = AppCore::in_memory().unwrap();
        app.install_bridge_credential_store(std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )))
        .unwrap();
        app.set_bridge_credential(Some("route-test-credential".into()), u64::MAX)
            .unwrap();
        for (method, path) in routes {
            let response = route_http_request(
                &app,
                HttpRequest {
                    method: (*method).to_string(),
                    path: (*path).to_string(),
                    query: Default::default(),
                    body: Vec::new(),
                    authorization: Some("Bearer route-test-credential".into()),
                },
            );
            assert_eq!(response.status, "404 Not Found", "route {method} {path}");
        }
    }

    #[cfg(not(feature = "assistant-e2e"))]
    #[test]
    fn e2e_routes_are_not_dispatchable_in_production_builds() {
        assert_routes_unavailable(&[
            ("POST", "/__e2e/actions"),
            ("GET", "/__e2e/events"),
            ("GET", "/__e2e/ready"),
        ]);
    }

    #[test]
    fn wiki_routes_rewrite_to_knowledge_routes() {
        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/api/wiki/manifest".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/manifest"
        );

        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/api/wiki/setup".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/setup"
        );

        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/api/wiki/export".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/export"
        );

        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/api/wiki/page".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/page"
        );

        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/api/wiki/write".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/write"
        );

        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/api/wiki/resolve-target".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/resolve-target"
        );

        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/api/wiki/events".to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            rewrite_knowledge_routes(request).path,
            "/api/knowledge/events"
        );
    }
}
