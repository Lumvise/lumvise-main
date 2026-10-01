use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::{
    AppCore, PROJECT_EXECUTION_HEARTBEAT_ENDPOINT, PROJECT_EXECUTION_NEXT_ENDPOINT,
    PROJECT_EXECUTION_PROGRESS_ENDPOINT, PROJECT_EXECUTION_REGISTER_ENDPOINT,
    PROJECT_EXECUTION_RESULT_ENDPOINT, PROJECT_EXECUTION_UNREGISTER_ENDPOINT,
    ProjectExecutionError,
};

const MAX_POLL_TIMEOUT_MS: u64 = 2_000;

pub(crate) fn project_execution_response(
    app: &AppCore,
    request: &HttpRequest,
) -> Option<HttpResponse> {
    let response = match (request.method.as_str(), request.path.as_str()) {
        ("POST", PROJECT_EXECUTION_REGISTER_ENDPOINT) => register(app, &request.body),
        ("POST", PROJECT_EXECUTION_NEXT_ENDPOINT) => next(app, &request.body),
        ("POST", PROJECT_EXECUTION_PROGRESS_ENDPOINT) => progress(app, &request.body),
        ("POST", PROJECT_EXECUTION_RESULT_ENDPOINT) => result(app, &request.body),
        ("POST", PROJECT_EXECUTION_HEARTBEAT_ENDPOINT) => heartbeat(app, &request.body),
        ("POST", PROJECT_EXECUTION_UNREGISTER_ENDPOINT) => unregister(app, &request.body),
        (_, path) if path.starts_with("/api/project-execution/") => error_response(
            "405 Method Not Allowed",
            "unsupported project execution endpoint",
        ),
        _ => return None,
    };
    Some(response)
}

fn register(app: &AppCore, body: &[u8]) -> HttpResponse {
    let request: RegisterRequest = match decode(body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let token = app.project_execution_control.register(
        app.project_execution(),
        &request.provider_id,
        &request.project_root,
        request.capabilities,
    );
    match token {
        Ok(connection_token) => json_response(
            "200 OK",
            json!({"provider_id": request.provider_id, "connection_token": connection_token,
                "lease_seconds": 10}),
        ),
        Err(error) => execution_error(error),
    }
}

fn next(app: &AppCore, body: &[u8]) -> HttpResponse {
    let request: AuthenticatedRequest = match decode(body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let timeout = Duration::from_millis(request.timeout_ms.min(MAX_POLL_TIMEOUT_MS));
    let command = app.project_execution_control.next(
        app.project_execution(),
        &request.provider_id,
        &request.connection_token,
        timeout,
    );
    match command {
        Ok(command) => json_response("200 OK", json!({"command": command})),
        Err(error) => execution_error(error),
    }
}

fn heartbeat(app: &AppCore, body: &[u8]) -> HttpResponse {
    let request: ProviderAuthentication = match decode(body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match authenticated_provider(app, &request) {
        Ok(_) => json_response("200 OK", json!({"renewed": true, "lease_seconds": 10})),
        Err(error) => execution_error(error),
    }
}

fn unregister(app: &AppCore, body: &[u8]) -> HttpResponse {
    let request: ProviderAuthentication = match decode(body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match app.project_execution_control.unregister(
        app.project_execution(),
        &request.provider_id,
        &request.connection_token,
    ) {
        Ok(unregistered) => json_response("200 OK", json!({"unregistered": unregistered})),
        Err(error) => execution_error(error),
    }
}

fn progress(app: &AppCore, body: &[u8]) -> HttpResponse {
    let request: ProgressRequest = match decode(body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let provider = authenticated_provider(app, &request.auth);
    let job = provider.and_then(|provider| {
        app.project_execution()
            .record_progress(&provider, &request.job_id, request.message)
    });
    job_response(job)
}

fn result(app: &AppCore, body: &[u8]) -> HttpResponse {
    let request: ResultRequest = match decode(body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let provider = authenticated_provider(app, &request.auth);
    let result = validated_result(&request);
    let job = provider.and_then(|provider| {
        result.and_then(|result| {
            app.project_execution()
                .record_result(&provider, &request.job_id, result)
        })
    });
    job_response(job)
}

fn authenticated_provider(
    app: &AppCore,
    request: &ProviderAuthentication,
) -> Result<String, ProjectExecutionError> {
    let provider = app
        .project_execution_control
        .authenticated_provider(&request.provider_id, &request.connection_token)?;
    app.project_execution().heartbeat(&provider)?;
    Ok(provider)
}

fn validated_result(
    request: &ResultRequest,
) -> Result<Result<Value, String>, ProjectExecutionError> {
    if request.ok && request.error.is_none() {
        return Ok(Ok(request.output.clone().unwrap_or(Value::Null)));
    }
    if !request.ok && request.output.is_none() {
        let error = request
            .error
            .clone()
            .filter(|value| !value.trim().is_empty());
        return error.map(Err).ok_or_else(invalid_terminal_result);
    }
    Err(invalid_terminal_result())
}

fn invalid_terminal_result() -> ProjectExecutionError {
    ProjectExecutionError::InvalidRequest {
        value: "inconsistent result fields".into(),
        expected: "ok result with output or failed result with non-empty error".into(),
    }
}

fn job_response(job: Result<crate::ProjectExecutionJob, ProjectExecutionError>) -> HttpResponse {
    match job {
        Ok(job) => json_response("200 OK", json!({"job": job})),
        Err(error) => execution_error(error),
    }
}

fn execution_error(error: ProjectExecutionError) -> HttpResponse {
    let status = match error {
        ProjectExecutionError::ProviderUnavailable { .. } => "503 Service Unavailable",
        ProjectExecutionError::JobNotFound { .. } => "404 Not Found",
        ProjectExecutionError::StateUnavailable => "500 Internal Server Error",
        ProjectExecutionError::ProviderQueueFull { .. } => "429 Too Many Requests",
        ProjectExecutionError::TerminalJob { .. } => "409 Conflict",
        _ => "400 Bad Request",
    };
    error_response(status, error)
}

fn decode<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, HttpResponse> {
    serde_json::from_slice(body).map_err(|error| {
        error_response("400 Bad Request", format!("invalid request body: {error}"))
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRequest {
    provider_id: String,
    project_root: String,
    capabilities: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedRequest {
    provider_id: String,
    connection_token: String,
    #[serde(default)]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderAuthentication {
    provider_id: String,
    connection_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressRequest {
    #[serde(flatten)]
    auth: ProviderAuthentication,
    job_id: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultRequest {
    #[serde(flatten)]
    auth: ProviderAuthentication,
    job_id: String,
    ok: bool,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}
