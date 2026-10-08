//! HTTP handlers for MCP worker registration and lifecycle (contracts wire shapes).
//!
//! Serves `POST /api/mcp/register`, `POST /api/mcp/heartbeat`, and
//! `GET /api/mcp/instances` using `lumvise_contracts::mcp` types end to end —
//! the same types `lumvise-mcp-repo`'s app client serializes. No local wire
//! structs: shape drift between app and worker must be impossible here.

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::AppCore;
use lumvise_contracts::{
    ControlChannelDescriptor, HeartbeatRequest, McpCapability, McpInstanceListResponse,
    McpInstanceStatus, RegisterMcpRequest, RegisterMcpResponse, RegisteredMcpInstance,
};
use lumvise_db_core::{McpInstance, RelationalOperation, RelationalResult};
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;

/// Routes MCP session registry HTTP requests.
pub(super) fn mcp_session_registry_response(
    app: &AppCore,
    request: &HttpRequest,
) -> Option<HttpResponse> {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/api/mcp/register") => Some(register(app, request)),
        ("POST", "/api/mcp/heartbeat") => Some(heartbeat(app, request)),
        ("GET", "/api/mcp/instances") => Some(list_requested_instances(app, request)),
        _ => None,
    }
}

fn register(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let register_request = match serde_json::from_slice::<RegisterMcpRequest>(&request.body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return error_response(
                "400 Bad Request",
                format!("invalid register request: {error}"),
            );
        }
    };
    if register_request.instance_id.trim().is_empty()
        || register_request.project_root.trim().is_empty()
    {
        return error_response(
            "400 Bad Request",
            "instance_id and project_root must be non-empty",
        );
    }
    let capabilities_json = match serde_json::to_string(&register_request.capabilities) {
        Ok(json) => json,
        Err(error) => return error_response("400 Bad Request", error.to_string()),
    };
    let control_channel_json = match register_request
        .control_channel
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
    {
        Ok(json) => json,
        Err(error) => return error_response("400 Bad Request", error.to_string()),
    };
    let stored = app.relational.execute(
        RelationalOperation::RegisterMcpInstance {
            instance_id: register_request.instance_id,
            project_root: register_request.project_root,
            display_name: register_request.display_name.unwrap_or_default(),
            capabilities_json,
            control_channel_json,
        },
        &InvocationControl::sixty_seconds(),
    );
    match stored {
        Ok(RelationalResult::McpInstance(instance)) => remember_registration(app, instance),
        Ok(other) => error_response(
            "500 Internal Server Error",
            format!("register MCP returned unexpected persistence result: {other:?}"),
        ),
        Err(error) => error_response(
            "500 Internal Server Error",
            format!("failed to register MCP instance: {error}"),
        ),
    }
}

fn heartbeat(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let heartbeat_request = match serde_json::from_slice::<HeartbeatRequest>(&request.body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return error_response(
                "400 Bad Request",
                format!("invalid heartbeat request: {error}"),
            );
        }
    };
    let status = status_to_string(&heartbeat_request.status);
    let stored = app.relational.execute(
        RelationalOperation::HeartbeatMcpInstance {
            instance_id: heartbeat_request.instance_id,
            project_root: heartbeat_request.project_root,
            status,
        },
        &InvocationControl::sixty_seconds(),
    );
    match stored {
        Ok(RelationalResult::McpInstance(instance)) => remember_heartbeat(app, instance),
        Ok(other) => error_response(
            "500 Internal Server Error",
            format!("heartbeat MCP returned unexpected persistence result: {other:?}"),
        ),
        Err(error) => error_response(
            "404 Not Found",
            format!("failed to heartbeat MCP instance: {error}"),
        ),
    }
}

fn remember_registration(app: &AppCore, instance: McpInstance) -> HttpResponse {
    let Ok(mut membership) = app.mcp_session_membership.lock() else {
        return error_response(
            "500 Internal Server Error",
            "MCP membership lock unavailable",
        );
    };
    membership.insert(
        instance.instance_id.clone(),
        Some(instance.project_root.clone()),
    );
    accepted_response(instance)
}

fn remember_heartbeat(app: &AppCore, instance: McpInstance) -> HttpResponse {
    let Ok(mut membership) = app.mcp_session_membership.lock() else {
        return error_response(
            "500 Internal Server Error",
            "MCP membership lock unavailable",
        );
    };
    observe_heartbeat(&mut membership, &instance);
    accepted_response(instance)
}

fn observe_heartbeat(
    membership: &mut std::collections::HashMap<String, Option<String>>,
    instance: &McpInstance,
) {
    let observed = membership.get(&instance.instance_id);
    if observed.is_some_and(|root| {
        root.as_ref()
            .is_some_and(|root| root != &instance.project_root)
    }) {
        return;
    }
    if instance.status == "unavailable" {
        // Retired connections stay fenced against delayed traffic until explicit registration.
        membership.insert(instance.instance_id.clone(), None);
    } else if matches!(instance.status.as_str(), "ready" | "busy") && observed.is_none() {
        membership.insert(
            instance.instance_id.clone(),
            Some(instance.project_root.clone()),
        );
    }
}

fn list_requested_instances(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    match request.query.get("active").map(String::as_str) {
        None | Some("false") => list_instances(app),
        Some("true") => list_active_instances(app),
        Some(value) => error_response(
            "400 Bad Request",
            format!("active query `{value}`: expected true or false"),
        ),
    }
}

fn list_instances(app: &AppCore) -> HttpResponse {
    list_selected_instances(app, false)
}

fn list_active_instances(app: &AppCore) -> HttpResponse {
    list_selected_instances(app, true)
}

fn list_selected_instances(app: &AppCore, active: bool) -> HttpResponse {
    let instances = match read_selected_instances(app, active) {
        Ok(instances) => instances,
        Err(message) => return error_response("500 Internal Server Error", message),
    };
    let instances = match filter_active_instances(app, instances, active) {
        Ok(instances) => instances,
        Err(response) => return response,
    };
    let response = McpInstanceListResponse {
        instances: instances.into_iter().map(registered_instance).collect(),
    };
    match serde_json::to_value(&response) {
        Ok(body) => json_response("200 OK", body),
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    }
}

fn read_selected_instances(app: &AppCore, active: bool) -> Result<Vec<McpInstance>, String> {
    let operation = if active {
        RelationalOperation::ActiveMcpInstances
    } else {
        RelationalOperation::AllMcpInstances
    };
    match app
        .relational
        .execute(operation, &InvocationControl::sixty_seconds())
    {
        Ok(RelationalResult::McpInstances(instances)) => Ok(instances),
        Ok(other) => Err(format!(
            "list MCP instances returned unexpected persistence result: {other:?}"
        )),
        Err(error) => Err(format!("failed to list MCP instances: {error}")),
    }
}

fn filter_active_instances(
    app: &AppCore,
    instances: Vec<McpInstance>,
    active: bool,
) -> Result<Vec<McpInstance>, HttpResponse> {
    if !active {
        return Ok(instances);
    }
    let membership = app.mcp_session_membership.lock().map_err(|_| {
        error_response(
            "500 Internal Server Error",
            "MCP membership lock unavailable",
        )
    })?;
    Ok(instances
        .into_iter()
        .filter(|instance| observed_live_instance(&membership, instance))
        .collect())
}

fn observed_live_instance(
    membership: &std::collections::HashMap<String, Option<String>>,
    instance: &McpInstance,
) -> bool {
    matches!(instance.status.as_str(), "ready" | "busy")
        && membership
            .get(&instance.instance_id)
            .and_then(Option::as_ref)
            == Some(&instance.project_root)
}

fn accepted_response(instance: McpInstance) -> HttpResponse {
    let response = RegisterMcpResponse {
        accepted: true,
        instance: registered_instance(instance),
    };
    match serde_json::to_value(&response) {
        Ok(body) => json_response("200 OK", body),
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    }
}

fn registered_instance(instance: McpInstance) -> RegisteredMcpInstance {
    RegisteredMcpInstance {
        capabilities: parse_capabilities(&instance.capabilities_json),
        status: parse_status(&instance.status),
        control_channel: parse_control_channel(instance.control_channel_json.as_deref()),
        instance_id: instance.instance_id,
        project_root: instance.project_root,
        display_name: instance.display_name,
        registered_at: instance.created_at,
        last_seen_at: instance.last_heartbeat_at,
    }
}

fn parse_capabilities(json: &str) -> Vec<McpCapability> {
    serde_json::from_str(json).unwrap_or_default()
}

fn parse_status(status: &str) -> McpInstanceStatus {
    serde_json::from_value(Value::String(status.to_string()))
        .unwrap_or(McpInstanceStatus::Unavailable)
}

fn parse_control_channel(json: Option<&str>) -> Option<ControlChannelDescriptor> {
    json.and_then(|value| serde_json::from_str(value).ok())
}

fn status_to_string(status: &McpInstanceStatus) -> String {
    match serde_json::to_value(status) {
        Ok(Value::String(value)) => value,
        _ => "unavailable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_db_core::{RelationalPersistence, RelationalReadiness};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct FakeMcpClient<'a> {
        app: &'a AppCore,
        id: &'a str,
    }

    impl FakeMcpClient<'_> {
        fn bind(&self, root: &str) {
            let response = self.app.route_app_bridge_request(
                "POST",
                "/api/mcp/register",
                Default::default(),
                serde_json::to_vec(
                    &json!({"instance_id":self.id,"project_root":root,"capabilities":[]}),
                )
                .unwrap(),
            );
            assert_eq!(response.0, 200, "{}", response.1);
            self.heartbeat(root, "ready");
        }
        fn heartbeat(&self, root: &str, status: &str) {
            let response = self.app.route_app_bridge_request(
                "POST",
                "/api/mcp/heartbeat",
                Default::default(),
                serde_json::to_vec(
                    &json!({"instance_id":self.id,"project_root":root,"status":status}),
                )
                .unwrap(),
            );
            assert_eq!(response.0, 200, "{}", response.1);
        }
    }

    fn observed_instances(app: &AppCore, active: bool) -> Value {
        let query = if active {
            std::collections::BTreeMap::from([("active".into(), "true".into())])
        } else {
            Default::default()
        };
        let (status, body) =
            app.route_app_bridge_request("GET", "/api/mcp/instances", query, vec![]);
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    struct FakeFreshnessPersistence {
        delegate: Arc<dyn RelationalPersistence>,
        expired: AtomicBool,
    }

    impl RelationalPersistence for FakeFreshnessPersistence {
        fn execute(
            &self,
            operation: RelationalOperation,
            control: &InvocationControl,
        ) -> lumvise_db_core::PersistenceResult<RelationalResult> {
            if matches!(operation, RelationalOperation::ActiveMcpInstances)
                && self.expired.load(Ordering::Acquire)
            {
                return Ok(RelationalResult::McpInstances(Vec::new()));
            }
            self.delegate.execute(operation, control)
        }
        fn readiness(&self) -> lumvise_db_core::PersistenceResult<RelationalReadiness> {
            self.delegate.readiness()
        }
    }

    #[test]
    fn mcp_session_registry_multi_client_disconnect_and_rebind_are_independent() {
        let app = AppCore::in_memory().unwrap();
        let one = FakeMcpClient {
            app: &app,
            id: "connection-one",
        };
        let two = FakeMcpClient {
            app: &app,
            id: "connection-two",
        };
        one.bind("/repo");
        two.bind("/repo");
        assert_eq!(
            observed_instances(&app, true)["instances"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        one.heartbeat("/repo", "unavailable");
        one.bind("/different");
        two.heartbeat("/repo", "unavailable");
        let active = observed_instances(&app, true);
        assert_eq!(active["instances"].as_array().unwrap().len(), 1);
        assert_eq!(active["instances"][0]["project_root"], "/different");
        one.heartbeat("/different", "unavailable");
        one.heartbeat("/different", "ready");
        assert_eq!(observed_instances(&app, true)["instances"], json!([]));
        assert_eq!(
            observed_instances(&app, false)["instances"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn mcp_session_registry_previous_runtime_rows_require_current_traffic_and_retirement_is_sticky()
    {
        let previous = AppCore::in_memory().unwrap();
        FakeMcpClient {
            app: &previous,
            id: "old-connection",
        }
        .bind("/repo");
        let mut current = AppCore::in_memory().unwrap();
        current.relational = previous.relational.clone();
        assert_eq!(
            observed_instances(&current, false)["instances"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(observed_instances(&current, true)["instances"], json!([]));
        let client = FakeMcpClient {
            app: &current,
            id: "old-connection",
        };
        client.heartbeat("/repo", "ready");
        assert_eq!(
            observed_instances(&current, true)["instances"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        client.heartbeat("/repo", "unavailable");
        client.heartbeat("/repo", "busy");
        assert_eq!(observed_instances(&current, true)["instances"], json!([]));
        client.bind("/repo");
        assert_eq!(
            observed_instances(&current, true)["instances"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn mcp_session_registry_active_query_obeys_portable_freshness_and_status() {
        let mut app = AppCore::in_memory().unwrap();
        let persistence = Arc::new(FakeFreshnessPersistence {
            delegate: app.relational.clone(),
            expired: AtomicBool::new(false),
        });
        app.relational = persistence.clone();
        let client = FakeMcpClient {
            app: &app,
            id: "connection",
        };
        let starting = app.route_app_bridge_request(
            "POST",
            "/api/mcp/register",
            Default::default(),
            serde_json::to_vec(
                &json!({"instance_id":"connection","project_root":"/repo","capabilities":[]}),
            )
            .unwrap(),
        );
        assert_eq!(starting.0, 200);
        assert_eq!(observed_instances(&app, true)["instances"], json!([]));
        client.heartbeat("/repo", "busy");
        assert_eq!(
            observed_instances(&app, true)["instances"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        persistence.expired.store(true, Ordering::Release);
        assert_eq!(observed_instances(&app, true)["instances"], json!([]));
        assert_eq!(
            observed_instances(&app, false)["instances"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let bad = app.route_app_bridge_request(
            "GET",
            "/api/mcp/instances",
            std::collections::BTreeMap::from([("active".into(), "yes".into())]),
            vec![],
        );
        assert_eq!(bad.0, 400);
    }

    fn post(path: &str, body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: path.to_string(),
            query: Default::default(),
            body: serde_json::to_vec(&body).unwrap(),
            authorization: None,
        }
    }

    #[test]
    fn register_roundtrips_contract_shapes() {
        let app = AppCore::in_memory().unwrap();
        let request = post(
            "/api/mcp/register",
            json!({
                "instance_id": "mcp-1",
                "project_root": "/repo",
                "display_name": "worker",
                "capabilities": ["semantic_indexing"],
                "control_channel": null
            }),
        );
        let response = register(&app, &request);
        assert_eq!(response.status, "200 OK");
        let body: Value = serde_json::from_slice(response.buffered_bytes().unwrap()).unwrap();
        assert_eq!(body["accepted"], true);
        assert_eq!(body["instance"]["instance_id"], "mcp-1");
        assert_eq!(body["instance"]["status"], "starting");
        assert_eq!(body["instance"]["capabilities"][0], "semantic_indexing");
    }

    #[test]
    fn register_rejects_empty_identity() {
        let app = AppCore::in_memory().unwrap();
        let request = post(
            "/api/mcp/register",
            json!({"instance_id": "", "project_root": "/repo", "capabilities": []}),
        );
        assert_eq!(register(&app, &request).status, "400 Bad Request");
    }

    #[test]
    fn heartbeat_updates_status_and_404s_for_unknown() {
        let app = AppCore::in_memory().unwrap();
        let unknown = post(
            "/api/mcp/heartbeat",
            json!({"instance_id": "missing", "project_root": "/repo", "status": "ready"}),
        );
        assert_eq!(heartbeat(&app, &unknown).status, "404 Not Found");
        let register_request = post(
            "/api/mcp/register",
            json!({"instance_id": "mcp-1", "project_root": "/repo", "capabilities": []}),
        );
        register(&app, &register_request);
        let beat = post(
            "/api/mcp/heartbeat",
            json!({"instance_id": "mcp-1", "project_root": "/repo", "status": "ready"}),
        );
        let response = heartbeat(&app, &beat);
        assert_eq!(response.status, "200 OK");
        let body: Value = serde_json::from_slice(response.buffered_bytes().unwrap()).unwrap();
        assert_eq!(body["instance"]["status"], "ready");
    }

    #[test]
    fn list_returns_empty_when_no_instances() {
        let app = AppCore::in_memory().unwrap();
        let response = list_instances(&app);
        assert_eq!(response.status, "200 OK");
        let body: Value = serde_json::from_slice(response.buffered_bytes().unwrap()).unwrap();
        assert_eq!(body["instances"].as_array().unwrap().len(), 0);
    }
}
