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
        ("GET", "/api/mcp/instances") => Some(list_instances(app)),
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
        Ok(RelationalResult::McpInstance(instance)) => accepted_response(instance),
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
        Ok(RelationalResult::McpInstance(instance)) => accepted_response(instance),
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

fn list_instances(app: &AppCore) -> HttpResponse {
    match app.relational.execute(
        RelationalOperation::AllMcpInstances,
        &InvocationControl::sixty_seconds(),
    ) {
        Ok(RelationalResult::McpInstances(instances)) => {
            let response = McpInstanceListResponse {
                instances: instances.into_iter().map(registered_instance).collect(),
            };
            match serde_json::to_value(&response) {
                Ok(body) => json_response("200 OK", body),
                Err(error) => error_response("500 Internal Server Error", error.to_string()),
            }
        }
        Ok(other) => error_response(
            "500 Internal Server Error",
            format!("list MCP instances returned unexpected persistence result: {other:?}"),
        ),
        Err(error) => error_response(
            "500 Internal Server Error",
            format!("failed to list MCP instances: {error}"),
        ),
    }
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
    use serde_json::json;

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
