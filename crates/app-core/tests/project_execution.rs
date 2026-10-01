#![cfg(feature = "desktop-app")]
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::Arc,
};

use lumvise_app_core::{
    AcquireResult, ActivationRequest, AppCore, AppCoreDesktopBridge, AppRuntimeCoordinator,
    PROJECT_EXECUTION_HEARTBEAT_ENDPOINT, PROJECT_EXECUTION_NEXT_ENDPOINT,
    PROJECT_EXECUTION_UNREGISTER_ENDPOINT, ProjectExecutionCommand, ProjectExecutionError,
    ProjectExecutionPriority, ProjectExecutionRequest, ProjectExecutionStatus, RuntimeConnection,
};
use serde_json::json;

const CAPABILITY: &str = "semantic.generate_functional_artifacts.v1";

struct TestBridge {
    _runtime_root: tempfile::TempDir,
    _bridge: AppCoreDesktopBridge,
    connection: RuntimeConnection,
}

#[derive(Clone)]
struct BridgeEndpoint {
    base_url: String,
    credential: String,
}

impl TestBridge {
    fn new(app: Arc<AppCore>) -> Self {
        let runtime_root = tempfile::tempdir().expect("runtime root");
        let coordinator =
            AppRuntimeCoordinator::new(runtime_root.path().join("runtime"), |_| Ok(()));
        let owner = match coordinator
            .acquire_or_forward(ActivationRequest::default())
            .expect("runtime ownership")
        {
            AcquireResult::Owner(owner) => owner,
            AcquireResult::Forwarded(_) => panic!("test runtime unexpectedly forwarded"),
        };
        let bridge = AppCoreDesktopBridge::new(app, owner).expect("authenticated app bridge");
        let connection = bridge
            .runtime_connection()
            .cloned()
            .expect("ready runtime connection");
        Self {
            _runtime_root: runtime_root,
            _bridge: bridge,
            connection,
        }
    }

    fn base_url(&self) -> BridgeEndpoint {
        BridgeEndpoint {
            base_url: self.connection.app_bridge_base_url.clone(),
            credential: self.connection.app_bridge_credential.clone(),
        }
    }
}

#[test]
fn submit_routes_predefined_capability_to_exact_project_provider() {
    let app = AppCore::in_memory().expect("app core");
    let (commands, receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider("mcp-1", "/work/project", [CAPABILITY.into()], commands)
        .expect("register provider");

    let job = app
        .project_execution()
        .submit(request("builtin.knowledge", "artifact:fn:parse"))
        .expect("submit project execution");

    assert_eq!(
        receiver.recv().expect("provider command"),
        ProjectExecutionCommand::Execute {
            job_id: job.job_id,
            capability_id: CAPABILITY.into(),
            input: json!({"semantic_element_id": "fn:parse"}),
        }
    );
}

#[test]
fn submit_is_idempotent_per_requester_and_key() {
    let app = AppCore::in_memory().expect("app core");
    let (commands, receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider("mcp-1", "/work/project", [CAPABILITY.into()], commands)
        .expect("register provider");
    let request = request("builtin.knowledge", "artifact:fn:parse");

    let first = app
        .project_execution()
        .submit(request.clone())
        .expect("first");
    let second = app.project_execution().submit(request).expect("second");

    assert_eq!(first.job_id, second.job_id);
    assert_eq!(receiver.try_iter().count(), 1);
}

#[test]
fn failed_idempotent_job_can_be_explicitly_resubmitted() {
    let app = AppCore::in_memory().expect("app core");
    let (commands, receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider("mcp-1", "/work/project", [CAPABILITY.into()], commands)
        .expect("register provider");
    let request = request("builtin.knowledge", "artifact:fn:parse");
    let first = app
        .project_execution()
        .submit(request.clone())
        .expect("first");
    receiver.recv().expect("first command");
    app.project_execution()
        .record_result("mcp-1", &first.job_id, Err("generation failed".into()))
        .expect("record failure");

    let retried = app.project_execution().submit(request).expect("retry");

    assert_ne!(retried.job_id, first.job_id);
    assert!(matches!(
        receiver.recv().expect("retry command"),
        ProjectExecutionCommand::Execute { job_id, .. } if job_id == retried.job_id
    ));
}

#[test]
fn provider_reconnect_redispatches_active_job_with_original_input() {
    let app = AppCore::in_memory().expect("app core");
    let (first_commands, _first_receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider(
            "mcp-1",
            "/work/project",
            [CAPABILITY.into()],
            first_commands,
        )
        .expect("register first provider");
    let job = app
        .project_execution()
        .submit(request("builtin.knowledge", "artifact:fn:parse"))
        .expect("submit project execution");

    let (reconnected_commands, reconnected_receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider(
            "mcp-2",
            "/work/project",
            [CAPABILITY.into()],
            reconnected_commands,
        )
        .expect("register replacement provider");

    assert_eq!(
        reconnected_receiver.recv().expect("redispatched command"),
        ProjectExecutionCommand::Execute {
            job_id: job.job_id,
            capability_id: CAPABILITY.into(),
            input: json!({"semantic_element_id": "fn:parse"}),
        }
    );
}

#[test]
fn submit_rejects_unregistered_capability_without_dispatch() {
    let app = AppCore::in_memory().expect("app core");
    let (commands, receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider("mcp-1", "/work/project", ["other.v1".into()], commands)
        .expect("register provider");

    let error = app
        .project_execution()
        .submit(request("builtin.knowledge", "artifact:fn:parse"))
        .expect_err("capability must be unavailable");

    assert!(matches!(
        error,
        ProjectExecutionError::ProviderUnavailable { .. }
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn provider_result_is_visible_only_to_original_requester() {
    let app = AppCore::in_memory().expect("app core");
    let (commands, _receiver) = mpsc::sync_channel(32);
    app.project_execution()
        .register_provider("mcp-1", "/work/project", [CAPABILITY.into()], commands)
        .expect("register provider");
    let job = app
        .project_execution()
        .submit(request("builtin.knowledge", "artifact:fn:parse"))
        .expect("submit");
    app.project_execution()
        .record_result("mcp-1", &job.job_id, Ok(json!({"artifacts": []})))
        .expect("record provider result");

    let visible = app
        .project_execution()
        .status("builtin.knowledge", &job.job_id)
        .expect("owned status");
    let hidden = app.project_execution().status("other.plugin", &job.job_id);

    assert_eq!(visible.status, ProjectExecutionStatus::Succeeded);
    assert!(matches!(
        hidden,
        Err(ProjectExecutionError::RequesterMismatch { .. })
    ));
}
#[test]
fn tcp_control_adapter_round_trips_provider_command_and_result() {
    let app = Arc::new(AppCore::in_memory().expect("app core"));
    let server = TestBridge::new(Arc::clone(&app));
    let registered = post_json(
        server.base_url(),
        "/api/project-execution/providers/register",
        json!({"provider_id": "mcp-1", "project_root": "/work/project",
            "capabilities": [CAPABILITY]}),
    );
    let token = registered["connection_token"]
        .as_str()
        .expect("connection token");
    let job = app
        .project_execution()
        .submit(request("builtin.knowledge", "artifact:fn:parse"))
        .expect("submit");

    let next = post_json(
        server.base_url(),
        "/api/project-execution/providers/next",
        json!({"provider_id": "mcp-1", "connection_token": token, "timeout_ms": 10}),
    );
    post_json(
        server.base_url(),
        "/api/project-execution/providers/result",
        json!({"provider_id": "mcp-1", "connection_token": token,
            "job_id": job.job_id, "ok": true, "output": {"artifacts": []}}),
    );

    assert_eq!(next["command"]["capability_id"], CAPABILITY);
    assert_eq!(
        app.project_execution()
            .status("builtin.knowledge", &job.job_id)
            .expect("completed job")
            .status,
        ProjectExecutionStatus::Succeeded
    );
}

#[test]
fn saturated_long_polls_do_not_block_app_bridge_catalog_control() {
    let app = Arc::new(AppCore::in_memory().expect("app core"));
    let server = TestBridge::new(Arc::clone(&app));
    let registered = post_json(
        server.base_url(),
        "/api/project-execution/providers/register",
        json!({"provider_id": "poll-provider", "project_root": "/work/project",
            "capabilities": [CAPABILITY]}),
    );
    let token = registered["connection_token"].as_str().unwrap().to_owned();
    let (sent_tx, sent_rx) = mpsc::sync_channel(8);
    let mut polls = Vec::new();
    for _ in 0..8 {
        let base_url = server.base_url().to_owned();
        let token = token.clone();
        let sent = sent_tx.clone();
        polls.push(std::thread::spawn(move || {
            post_json_after_send(
                &base_url,
                PROJECT_EXECUTION_NEXT_ENDPOINT,
                json!({"provider_id": "poll-provider", "connection_token": token,
                    "timeout_ms": 500}),
                sent,
            )
        }));
    }
    drop(sent_tx);
    for _ in 0..8 {
        sent_rx.recv().expect("poll request sent");
    }

    let started = Instant::now();
    let surface = get_json(server.base_url(), "/api/mcp/plugins/surface");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(surface["generation"].is_string());
    for poll in polls {
        poll.join().unwrap();
    }
}

#[test]
fn heartbeat_renews_and_unregister_wakes_provider_long_poll() {
    let app = Arc::new(AppCore::in_memory().expect("app core"));
    let server = TestBridge::new(app);
    let registered = post_json(
        server.base_url(),
        "/api/project-execution/providers/register",
        json!({"provider_id": "leased-provider", "project_root": "/work/project",
            "capabilities": [CAPABILITY]}),
    );
    let token = registered["connection_token"].as_str().unwrap().to_owned();
    let heartbeat = post_json(
        server.base_url(),
        PROJECT_EXECUTION_HEARTBEAT_ENDPOINT,
        json!({"provider_id": "leased-provider", "connection_token": token}),
    );
    assert_eq!(heartbeat["renewed"], true);

    let (sent_tx, sent_rx) = mpsc::sync_channel(1);
    let base_url = server.base_url().to_owned();
    let poll_token = token.clone();
    let poll = std::thread::spawn(move || {
        post_json_after_send(
            &base_url,
            PROJECT_EXECUTION_NEXT_ENDPOINT,
            json!({"provider_id": "leased-provider", "connection_token": poll_token,
                "timeout_ms": 2000}),
            sent_tx,
        )
    });
    sent_rx.recv().expect("long poll sent");
    let started = Instant::now();
    let unregistered = post_json(
        server.base_url(),
        PROJECT_EXECUTION_UNREGISTER_ENDPOINT,
        json!({"provider_id": "leased-provider", "connection_token": token}),
    );
    assert_eq!(unregistered["unregistered"], true);
    let response = poll.join().expect("poll joined");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(response["error"].is_string());
}

fn request(requester_id: &str, idempotency_key: &str) -> ProjectExecutionRequest {
    ProjectExecutionRequest {
        requester_id: requester_id.into(),
        project_root: "/work/project".into(),
        capability_id: CAPABILITY.into(),
        idempotency_key: idempotency_key.into(),
        input: json!({"semantic_element_id": "fn:parse"}),
        priority: ProjectExecutionPriority::default(),
    }
}

fn post_json(endpoint: BridgeEndpoint, path: &str, body: serde_json::Value) -> serde_json::Value {
    let authority = endpoint
        .base_url
        .strip_prefix("http://")
        .expect("HTTP base URL");
    let body = serde_json::to_vec(&body).expect("serialize HTTP body");
    let mut stream = TcpStream::connect(authority).expect("connect HTTP server");
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        endpoint.credential,
        body.len()
    )
    .expect("write HTTP headers");
    stream.write_all(&body).expect("write HTTP body");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read HTTP response");
    let (head, body) = response.split_once("\r\n\r\n").expect("HTTP response body");
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "HTTP response: {head} {body}"
    );
    serde_json::from_str(body).expect("JSON response")
}
fn post_json_after_send(
    endpoint: &BridgeEndpoint,
    path: &str,
    body: serde_json::Value,
    sent: mpsc::SyncSender<()>,
) -> serde_json::Value {
    let authority = endpoint.base_url.strip_prefix("http://").unwrap();
    let body = serde_json::to_vec(&body).unwrap();
    let mut stream = TcpStream::connect(authority).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        endpoint.credential,
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    sent.send(()).unwrap();
    read_json_response(stream)
}

fn get_json(endpoint: BridgeEndpoint, path: &str) -> serde_json::Value {
    let authority = endpoint.base_url.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(authority).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        endpoint.credential
    )
    .unwrap();
    read_json_response(stream)
}

fn read_json_response(mut stream: TcpStream) -> serde_json::Value {
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (_head, body) = response.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}
