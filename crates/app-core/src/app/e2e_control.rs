//! E2E-only generic renderer action and observation journal.
//!
//! The module transports test control and evidence. It owns no Assistant
//! lifecycle, state transition, provider, voice, or canvas business rules.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Mutex;

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};

const E2E_ACTIONS_ENDPOINT: &str = "/__e2e/actions";
const E2E_EVENTS_ENDPOINT: &str = "/__e2e/events";
const E2E_READY_ENDPOINT: &str = "/__e2e/ready";
pub(crate) const E2E_RENDERER_EVENT_MEDIA_TYPE: &str = "application/x-lumvise-e2e-event+json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct E2eEvent {
    pub(crate) sequence: u64,
    pub(crate) correlation_id: String,
    pub(crate) kind: String,
    pub(crate) payload: Value,
}

#[derive(Debug, Default)]
pub(crate) struct E2eEventJournal {
    events: Mutex<Vec<E2eEvent>>,
}

impl E2eEventJournal {
    pub(crate) fn record(
        &self,
        correlation_id: &str,
        kind: &str,
        payload: Value,
    ) -> crate::Result<E2eEvent> {
        require_e2e_value(correlation_id, "non-empty E2E correlation id")?;
        require_e2e_value(kind, "non-empty E2E event kind")?;
        let mut events = self.lock_events()?;
        let sequence = u64::try_from(events.len())
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| {
                crate::AppCoreError::unsupported(events.len().to_string(), "u64 E2E sequence")
            })?;
        let event = E2eEvent {
            sequence,
            correlation_id: correlation_id.to_string(),
            kind: kind.to_string(),
            payload,
        };
        events.push(event.clone());
        Ok(event)
    }

    pub(crate) fn after(&self, sequence: u64) -> crate::Result<Vec<E2eEvent>> {
        Ok(self
            .lock_events()?
            .iter()
            .filter(|event| event.sequence > sequence)
            .cloned()
            .collect())
    }

    fn lock_events(&self) -> crate::Result<std::sync::MutexGuard<'_, Vec<E2eEvent>>> {
        self.events
            .lock()
            .map_err(|_| crate::AppCoreError::poisoned_mutex("e2e_events"))
    }
}

fn require_e2e_value(value: &str, expected: &str) -> crate::Result<()> {
    if value.trim().is_empty() {
        return Err(crate::AppCoreError::invalid_value(value, expected));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct E2eActionRequest {
    correlation_id: String,
    seed: u64,
    command: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererEventEnvelope {
    kind: String,
    #[serde(default)]
    payload: Value,
}

pub(crate) fn route_e2e_request(
    app: &crate::AppCore,
    request: &HttpRequest,
) -> Option<HttpResponse> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", E2E_READY_ENDPOINT) => Some(read_e2e_readiness(app)),
        ("POST", E2E_ACTIONS_ENDPOINT) => Some(accept_e2e_action(app, &request.body)),
        ("GET", E2E_EVENTS_ENDPOINT) => Some(read_e2e_events(app, request)),
        (_, E2E_ACTIONS_ENDPOINT | E2E_EVENTS_ENDPOINT | E2E_READY_ENDPOINT) => Some(
            error_response("405 Method Not Allowed", "unsupported E2E method"),
        ),
        _ => None,
    }
}

fn read_e2e_readiness(app: &crate::AppCore) -> HttpResponse {
    let frontend = match app.frontend.lock() {
        Ok(frontend) => frontend,
        Err(_) => return error_response("500 Internal Server Error", "frontend lock poisoned"),
    };
    let ready = frontend.state().app.lifecycle == lumvise_frontend_core::AppLifecycle::Spawned;
    let status = if ready {
        "200 OK"
    } else {
        "503 Service Unavailable"
    };
    let snapshot = match e2e_readiness_snapshot(app, ready) {
        Ok(snapshot) => snapshot,
        Err(error) => return error_response("500 Internal Server Error", error),
    };
    json_response(status, snapshot)
}

fn e2e_readiness_snapshot(app: &crate::AppCore, ready: bool) -> crate::Result<Value> {
    let plugin_processes = app.plugin_system().active_processes()?;
    Ok(json!({
        "ready": ready,
        "appProcessId": std::process::id(),
        "pluginProcesses": plugin_processes.into_iter().map(|process| json!({
                "pluginId": process.plugin_id,
                "processId": process.process_id,
        })).collect::<Vec<_>>(),
    }))
}

fn accept_e2e_action(app: &crate::AppCore, body: &[u8]) -> HttpResponse {
    let action = match parse_e2e_action(body).and_then(validate_e2e_action) {
        Ok(action) => action,
        Err(error) => return error_response("400 Bad Request", error),
    };
    if action.command.starts_with("runtime.") {
        return control_plugin_process(app, &action);
    }
    let frontend_action = json!({
        "action": "e2e.control",
        "correlationId": action.correlation_id,
        "seed": action.seed,
        "command": action.command,
        "payload": action.payload,
    });
    match app.enqueue_frontend_action(frontend_action) {
        Ok(()) => json_response("202 Accepted", json!({"accepted": true})),
        Err(error) => error_response("500 Internal Server Error", error),
    }
}

fn parse_e2e_action(body: &[u8]) -> Result<E2eActionRequest, String> {
    serde_json::from_slice(body).map_err(|error| {
        format!(
            "invalid E2E action body `{}`; expected action JSON: {error}",
            String::from_utf8_lossy(body)
        )
    })
}

fn validate_e2e_action(action: E2eActionRequest) -> Result<E2eActionRequest, String> {
    if action.correlation_id.trim().is_empty() || action.seed == 0 {
        return Err(format!(
            "invalid E2E identity `{}:{}`; expected non-empty correlationId and non-zero seed",
            action.correlation_id, action.seed
        ));
    }
    let supported = action.command.starts_with("renderer.")
        || matches!(
            action.command.as_str(),
            "runtime.start_plugin" | "runtime.stop_plugin"
        );
    if !supported {
        return Err(format!(
            "invalid E2E command `{}`; expected renderer.* or named runtime plugin command",
            action.command
        ));
    }
    Ok(action)
}

fn control_plugin_process(app: &crate::AppCore, action: &E2eActionRequest) -> HttpResponse {
    let plugin_id = match plugin_id(&action.payload) {
        Ok(plugin_id) => plugin_id,
        Err(error) => return error_response("400 Bad Request", error),
    };
    match action.command.as_str() {
        "runtime.stop_plugin" => stop_plugin_process(app, plugin_id, &action.payload),
        "runtime.start_plugin" => start_plugin_process(app, plugin_id, &action.payload),
        _ => error_response("400 Bad Request", "unsupported runtime E2E command"),
    }
}

fn stop_plugin_process(app: &crate::AppCore, plugin_id: &str, payload: &Value) -> HttpResponse {
    let expected = match required_process_id(payload, "expectedProcessId") {
        Ok(expected) => expected,
        Err(error) => return error_response("400 Bad Request", error),
    };
    if let Err(error) = require_current_process_id(app, plugin_id, expected) {
        return error_response("409 Conflict", error);
    }
    match app.plugin_system().stop(plugin_id) {
        Ok(()) => json_response("200 OK", json!({"stopped": true, "pluginId": plugin_id})),
        Err(error) => error_response("500 Internal Server Error", error),
    }
}

fn start_plugin_process(app: &crate::AppCore, plugin_id: &str, payload: &Value) -> HttpResponse {
    let previous = match required_process_id(payload, "previousProcessId") {
        Ok(previous) => previous,
        Err(error) => return error_response("400 Bad Request", error),
    };
    if let Err(error) = app.plugin_system().start(plugin_id) {
        return error_response("500 Internal Server Error", error);
    }
    let current = active_process_id(app, plugin_id).unwrap_or(0);
    if current == 0 || current == previous {
        return error_response(
            "500 Internal Server Error",
            format!(
                "plugin `{plugin_id}` restarted as PID `{current}`; expected non-zero PID distinct from `{previous}`"
            ),
        );
    }
    json_response(
        "200 OK",
        json!({"pluginId": plugin_id, "processId": current}),
    )
}

fn plugin_id(payload: &Value) -> Result<&str, String> {
    payload
        .get("pluginId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("invalid runtime payload `{payload}`; expected non-empty pluginId"))
}

fn required_process_id(payload: &Value, field: &str) -> Result<u32, String> {
    payload
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            format!("invalid runtime payload `{payload}`; expected non-zero u32 `{field}`")
        })
}

fn require_current_process_id(
    app: &crate::AppCore,
    plugin_id: &str,
    expected: u32,
) -> Result<(), String> {
    let current = active_process_id(app, plugin_id)?;
    if current == expected {
        return Ok(());
    }
    Err(format!(
        "stale plugin process `{plugin_id}:{expected}`; expected current PID `{current}`"
    ))
}

fn active_process_id(app: &crate::AppCore, plugin_id: &str) -> Result<u32, String> {
    app.plugin_system()
        .active_processes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|process| process.plugin_id == plugin_id)
        .map(|process| process.process_id)
        .ok_or_else(|| format!("plugin `{plugin_id}` has no ready process; expected active plugin"))
}

fn read_e2e_events(app: &crate::AppCore, request: &HttpRequest) -> HttpResponse {
    let after = match e2e_cursor(request) {
        Ok(after) => after,
        Err(error) => return error_response("400 Bad Request", error),
    };
    match app.e2e_event_journal.after(after) {
        Ok(events) => json_response("200 OK", json!({"events": events})),
        Err(error) => error_response("500 Internal Server Error", error),
    }
}

fn e2e_cursor(request: &HttpRequest) -> Result<u64, String> {
    request.query.get("after").map_or(Ok(0), |value| {
        value.parse::<u64>().map_err(|error| {
            format!("invalid E2E cursor `{value}`; expected unsigned sequence: {error}")
        })
    })
}

impl crate::AppCore {
    pub(crate) fn record_e2e_event(
        &self,
        correlation_id: &str,
        kind: &str,
        payload: Value,
    ) -> crate::Result<E2eEvent> {
        self.e2e_event_journal.record(correlation_id, kind, payload)
    }
}

pub(crate) fn record_encoded_renderer_event(
    app: &crate::AppCore,
    correlation_id: &str,
    bytes: &[u8],
) -> crate::Result<Value> {
    let envelope: RendererEventEnvelope = serde_json::from_slice(bytes).map_err(|error| {
        crate::AppCoreError::invalid_value(
            format!("{}: {error}", String::from_utf8_lossy(bytes)),
            "renderer E2E event JSON with kind and payload",
        )
    })?;
    let event = app.record_e2e_event(correlation_id, &envelope.kind, envelope.payload)?;
    serde_json::to_value(event).map_err(|error| {
        crate::AppCoreError::invalid_value(error.to_string(), "serializable E2E event")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn journal_assigns_monotonic_sequence_and_reads_after_cursor() {
        let journal = E2eEventJournal::default();
        let first = journal
            .record("scenario-7", "renderer.ready", json!({}))
            .unwrap();
        let second = journal
            .record("scenario-7", "capture.pcm", json!({"samples": 19200}))
            .unwrap();

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(journal.after(1).unwrap(), vec![second]);
    }

    #[test]
    fn journal_rejects_empty_identity_and_kind() {
        let journal = E2eEventJournal::default();

        assert!(journal.record("", "renderer.ready", json!({})).is_err());
        assert!(journal.record("scenario-7", "", json!({})).is_err());
        assert!(journal.after(0).unwrap().is_empty());
    }

    #[test]
    fn route_accepts_renderer_action_and_exposes_recorded_events() {
        let app = crate::AppCore::in_memory().unwrap();
        let action = HttpRequest {
            method: "POST".to_string(),
            path: E2E_ACTIONS_ENDPOINT.to_string(),
            query: Default::default(),
            body: serde_json::to_vec(&json!({
                "correlationId": "scenario-7",
                "seed": 7,
                "command": "renderer.configure_demo_voice",
                "payload": {"segments": 1}
            }))
            .unwrap(),
            authorization: None,
        };

        assert_eq!(
            route_e2e_request(&app, &action).unwrap().status,
            "202 Accepted"
        );
        assert_eq!(
            app.drain_frontend_actions().unwrap()[0]["action"],
            "e2e.control"
        );
        app.record_e2e_event("scenario-7", "renderer.ready", json!({}))
            .unwrap();
        let events = HttpRequest {
            method: "GET".to_string(),
            path: E2E_EVENTS_ENDPOINT.to_string(),
            query: [("after".to_string(), "0".to_string())].into(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(route_e2e_request(&app, &events).unwrap().status, "200 OK");
    }

    #[test]
    fn route_rejects_non_renderer_command_and_invalid_cursor() {
        let app = crate::AppCore::in_memory().unwrap();
        let action = HttpRequest {
            method: "POST".to_string(),
            path: E2E_ACTIONS_ENDPOINT.to_string(),
            query: Default::default(),
            body: serde_json::to_vec(&json!({
                "correlationId": "scenario-7", "seed": 7,
                "command": "assistant.force_state"
            }))
            .unwrap(),
            authorization: None,
        };
        assert_eq!(
            route_e2e_request(&app, &action).unwrap().status,
            "400 Bad Request"
        );
        let events = HttpRequest {
            method: "GET".to_string(),
            path: E2E_EVENTS_ENDPOINT.to_string(),
            query: [("after".to_string(), "nope".to_string())].into(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            route_e2e_request(&app, &events).unwrap().status,
            "400 Bad Request"
        );
    }

    #[test]
    fn runtime_plugin_control_is_generic_and_rejects_absent_identity() {
        let app = crate::AppCore::in_memory().unwrap();
        let missing_id = HttpRequest {
            method: "POST".to_string(),
            path: E2E_ACTIONS_ENDPOINT.to_string(),
            query: Default::default(),
            body: serde_json::to_vec(&json!({
                "correlationId": "scenario-7", "seed": 7,
                "command": "runtime.start_plugin", "payload": {}
            }))
            .unwrap(),
            authorization: None,
        };
        assert_eq!(
            route_e2e_request(&app, &missing_id).unwrap().status,
            "400 Bad Request"
        );

        let absent_plugin = HttpRequest {
            body: serde_json::to_vec(&json!({
                "correlationId": "scenario-7", "seed": 7,
                "command": "runtime.start_plugin",
                "payload": {"pluginId": "absent.plugin", "previousProcessId": 1}
            }))
            .unwrap(),
            ..missing_id
        };
        assert_eq!(
            route_e2e_request(&app, &absent_plugin).unwrap().status,
            "500 Internal Server Error"
        );
        assert!(app.drain_frontend_actions().unwrap().is_empty());
    }

    #[test]
    fn encoded_renderer_event_is_validated_and_appended() {
        let app = crate::AppCore::in_memory().unwrap();
        let event = record_encoded_renderer_event(
            &app,
            "scenario-7",
            br#"{"kind":"capture.pcm_submitted","payload":{"samples":19200}}"#,
        )
        .unwrap();

        assert_eq!(event["sequence"], 1);
        assert_eq!(event["kind"], "capture.pcm_submitted");
        assert!(record_encoded_renderer_event(&app, "scenario-7", b"not-json").is_err());
    }

    #[test]
    fn readiness_requires_spawned_frontend_lifecycle() {
        let app = crate::AppCore::in_memory().unwrap();
        let request = HttpRequest {
            method: "GET".to_string(),
            path: E2E_READY_ENDPOINT.to_string(),
            query: Default::default(),
            body: Vec::new(),
            authorization: None,
        };
        assert_eq!(
            route_e2e_request(&app, &request).unwrap().status,
            "503 Service Unavailable"
        );

        app.frontend
            .lock()
            .unwrap()
            .spawn_app(lumvise_frontend_core::WorkArea::new(0, 0, 1000, 800, 1.0))
            .unwrap();
        assert_eq!(route_e2e_request(&app, &request).unwrap().status, "200 OK");
        let body = e2e_readiness_snapshot(&app, true).unwrap();
        assert_eq!(body["appProcessId"], std::process::id());
        assert_eq!(body["pluginProcesses"], json!([]));
    }
}
