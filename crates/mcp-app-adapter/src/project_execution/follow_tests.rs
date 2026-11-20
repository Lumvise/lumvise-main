//! The provider follows the project an MCP connection is bound to, through a fake app bridge
//! that records every provider and binding request.

use super::*;
use crate::product::{McpIdentity, open_mcp_server};
use lumvise_app_core::{AcquireResult, ActivationRequest, AppRuntimeCoordinator, OwnerLease};
use lumvise_mcp_core::LumviseMcpServer;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MCP_REGISTER_ENDPOINT: &str = "/api/mcp/register";
const MCP_HEARTBEAT_ENDPOINT: &str = "/api/mcp/heartbeat";

struct Request {
    path: String,
    body: Value,
    arrived: Instant,
}

/// Scripted refusals. Registrations are numbered from 1 in arrival order.
#[derive(Default)]
struct Script {
    /// Roots refused on every registration.
    rejected: Vec<String>,
    /// Registration numbers refused regardless of root.
    refused_registrations: Vec<usize>,
    /// The first accepted connection fails its next poll once, forcing a reconnect.
    drop_first_connection: bool,
}

#[derive(Default)]
struct ServeState {
    tokens: u64,
    registrations: usize,
    drop_next: bool,
}

/// Accepts provider registrations unless the script refuses them.
struct FakeAppBridge {
    base_url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeAppBridge {
    fn start(rejected: Vec<String>) -> Self {
        Self::start_scripted(Script {
            rejected,
            ..Script::default()
        })
    }

    fn start_scripted(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, halt) = (Arc::clone(&requests), Arc::clone(&stop));
        let worker = std::thread::spawn(move || serve(listener, &log, &halt, &script));
        Self {
            base_url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    /// Provider registrations and unregistrations in arrival order, e.g. `register:/work/a`.
    fn provider_events(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|request| match request.path.as_str() {
                REGISTER_ENDPOINT => Some(format!(
                    "register:{}",
                    request.body["project_root"].as_str().unwrap()
                )),
                UNREGISTER_ENDPOINT => Some(format!(
                    "unregister:{}",
                    request.body["connection_token"].as_str().unwrap()
                )),
                _ => None,
            })
            .collect()
    }

    /// Arrival time of every provider registration, in order.
    fn provider_register_times(&self) -> Vec<Instant> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path == REGISTER_ENDPOINT)
            .map(|request| request.arrived)
            .collect()
    }

    fn wait_for_provider_events(&self, done: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&self.provider_events()) {
            assert!(
                Instant::now() < deadline,
                "expected provider events not observed: {:?}",
                self.provider_events()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for FakeAppBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn serve(listener: TcpListener, log: &Mutex<Vec<Request>>, stop: &AtomicBool, script: &Script) {
    let mut state = ServeState::default();
    while !stop.load(Ordering::Acquire) {
        let Ok((mut stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        // Accepted sockets inherit the listener's non-blocking mode on macOS.
        stream.set_nonblocking(false).unwrap();
        let (path, body) = read_request(&stream);
        let arrived = Instant::now();
        let (status, reply) = answer(&path, &body, script, &mut state);
        let reply = reply.to_string();
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
            reply.len()
        )
        .unwrap();
        log.lock().unwrap().push(Request {
            path,
            body,
            arrived,
        });
    }
}

fn read_request(stream: &TcpStream) -> (String, Value) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut content_length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    let target = request_line.split_whitespace().nth(1).unwrap();
    let path = target.split('?').next().unwrap().to_owned();
    // Bodyless catalog reads (GET plugins/surface) reach the fake too; they get 404.
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (path, body)
}

fn answer(
    path: &str,
    body: &Value,
    script: &Script,
    state: &mut ServeState,
) -> (&'static str, Value) {
    match path {
        MCP_REGISTER_ENDPOINT => ("200 OK", acknowledgement(body, json!("starting"))),
        MCP_HEARTBEAT_ENDPOINT => ("200 OK", acknowledgement(body, body["status"].clone())),
        REGISTER_ENDPOINT => {
            state.registrations += 1;
            let refused = script.refused_registrations.contains(&state.registrations)
                || script
                    .rejected
                    .iter()
                    .any(|root| body["project_root"].as_str() == Some(root.as_str()));
            if refused {
                return ("400 Bad Request", json!({"error": "unknown project"}));
            }
            state.tokens += 1;
            state.drop_next = script.drop_first_connection && state.tokens == 1;
            (
                "200 OK",
                json!({"connection_token": format!("token-{}", state.tokens)}),
            )
        }
        NEXT_ENDPOINT => {
            if std::mem::take(&mut state.drop_next) {
                return (
                    "500 Internal Server Error",
                    json!({"error": "connection dropped"}),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
            ("200 OK", json!({"command": null}))
        }
        UNREGISTER_ENDPOINT | HEARTBEAT_ENDPOINT => ("200 OK", json!({})),
        _ => ("404 Not Found", json!({"error": "unexpected endpoint"})),
    }
}

fn acknowledgement(body: &Value, status: Value) -> Value {
    json!({"accepted": true, "instance": {
        "instance_id": body["instance_id"],
        "project_root": body["project_root"],
        "display_name": "",
        "capabilities": [],
        "status": status,
        "registered_at": "",
        "last_seen_at": "",
        "control_channel": null,
    }})
}

/// Owns the runtime root and a ready app-bridge owner lease for the fake bridge.
struct Fixture {
    bridge: FakeAppBridge,
    app_bridge: AppBridgeConfig,
    _lease: OwnerLease,
    _runtime: tempfile::TempDir,
}

impl Fixture {
    fn start(rejected: Vec<String>) -> Self {
        Self::with_bridge(FakeAppBridge::start(rejected))
    }

    fn start_scripted(script: Script) -> Self {
        Self::with_bridge(FakeAppBridge::start_scripted(script))
    }

    fn with_bridge(bridge: FakeAppBridge) -> Self {
        let runtime = tempfile::tempdir().unwrap();
        let coordinator = AppRuntimeCoordinator::new(runtime.path(), |_| Ok(()));
        let AcquireResult::Owner(mut lease) = coordinator
            .acquire_or_forward(ActivationRequest::default())
            .unwrap()
        else {
            panic!("expected runtime owner");
        };
        lease.mark_ready(bridge.base_url.clone()).unwrap();
        let app_bridge = AppBridgeConfig::discovery().with_runtime_root(runtime.path());
        Self {
            bridge,
            app_bridge,
            _lease: lease,
            _runtime: runtime,
        }
    }

    fn connect(&self, project_root: Option<String>) -> Connection {
        self.open(project_root, None)
    }

    /// Starts a client launched inside `launch_folder` with no explicit `--project-root`.
    fn launch_in(&self, launch_folder: String) -> Connection {
        self.open(None, Some(launch_folder))
    }

    fn open(&self, project_root: Option<String>, default_root: Option<String>) -> Connection {
        let identity = McpIdentity {
            engine: "codex".into(),
            instance_id: "client-under-test".into(),
        };
        let (server, follower) = open_mcp_server(
            self.app_bridge.clone(),
            identity,
            project_root,
            default_root,
        );
        Connection {
            server,
            _follower: follower,
        }
    }
}

/// Declared server-first: the follower joins only after the server drops its root sender.
struct Connection {
    server: LumviseMcpServer,
    _follower: Option<ProjectExecutionFollower>,
}

impl Connection {
    fn initialize(&self) {
        let reply = self.call(
            1,
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "follow-test", "version": "0.0.0"},
            }),
        );
        assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");
        let reply = self
            .server
            .handle_json_line(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
            )
            .unwrap();
        assert!(reply.is_none(), "client initialization needs no reply");
    }

    fn call(&self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let reply = self
            .server
            .handle_json_line(&request.to_string())
            .unwrap()
            .expect("request reply");
        serde_json::from_str(&reply).unwrap()
    }

    /// Selects a project and returns the binding status the tool reported.
    fn select(&self, project_root: Value) -> String {
        let reply = self.call(
            2,
            "tools/call",
            json!({"name":"set_current_project","arguments":{"project_root":project_root}}),
        );
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("set_current_project failed: {reply}"));
        let output: Value = serde_json::from_str(text).unwrap();
        output["binding_status"].as_str().unwrap().to_owned()
    }
}

fn path_text(path: &std::path::Path) -> String {
    path.to_str().unwrap().to_owned()
}

#[test]
fn selecting_a_project_registers_its_provider() {
    let fixture = Fixture::start(vec![]);
    let project = tempfile::tempdir().unwrap();
    let root = path_text(project.path());
    let connection = fixture.connect(None);
    connection.initialize();

    assert_eq!(connection.select(json!(root)), "ready");

    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{root}")));
}

#[test]
fn rebinding_unregisters_the_old_root_before_registering_the_new_one() {
    let fixture = Fixture::start(vec![]);
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (first, second) = (path_text(first.path()), path_text(second.path()));
    let connection = fixture.connect(None);
    connection.initialize();

    assert_eq!(connection.select(json!(first)), "ready");
    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{first}")));
    assert_eq!(connection.select(json!(second)), "ready");
    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{second}")));

    // token-1 is the provider that served the first root.
    assert_eq!(
        fixture.bridge.provider_events(),
        [
            format!("register:{first}"),
            "unregister:token-1".to_owned(),
            format!("register:{second}"),
        ]
    );
}

#[test]
fn rejected_provider_registration_is_not_fatal_to_the_server() {
    let project = tempfile::tempdir().unwrap();
    let root = path_text(project.path());
    let fixture = Fixture::start(vec![root.clone()]);
    let connection = fixture.connect(None);
    connection.initialize();

    assert_eq!(connection.select(json!(root)), "ready");
    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{root}")));

    let tools = connection.call(3, "tools/list", json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "set_current_project"),
        "server must keep answering after a rejected registration: {tools}"
    );
}

#[test]
fn explicit_project_root_registers_at_startup() {
    let project = tempfile::tempdir().unwrap();
    let root = path_text(project.path());
    let fixture = Fixture::start(vec![]);

    // No MCP initialization or tool call: the explicit root serves from startup.
    let _connection = fixture.connect(Some(root.clone()));

    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{root}")));
}

#[test]
fn unbound_connection_registers_no_provider() {
    let fixture = Fixture::start(vec![]);
    let project = tempfile::tempdir().unwrap();
    let root = path_text(project.path());
    let connection = fixture.connect(None);
    connection.initialize();

    assert_eq!(connection.select(Value::Null), "unbound");
    assert_eq!(connection.select(json!(root)), "ready");
    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{root}")));

    // Nothing was registered while unbound: the first provider event is the bound root.
    assert_eq!(
        fixture.bridge.provider_events(),
        [format!("register:{root}")]
    );
}

#[test]
fn launch_folder_registers_its_provider_at_startup() {
    let project = tempfile::tempdir().unwrap();
    let root = path_text(project.path());
    let fixture = Fixture::start(vec![]);

    // The client is never initialized or bound: the launch folder serves from startup.
    let _connection = fixture.launch_in(root.clone());

    fixture
        .bridge
        .wait_for_provider_events(|events| events.contains(&format!("register:{root}")));
}

/// Polls until `done` holds, failing the test after five seconds.
fn eventually(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "condition not reached in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn refused_registrations_back_off_and_a_later_success_restarts_the_schedule() {
    let project = tempfile::tempdir().unwrap();
    let root = path_text(project.path());
    // Registrations 1-3 and 5 are refused. Registration 4 is accepted, its first poll fails,
    // and registration 6 is accepted and stays connected.
    let fixture = Fixture::start_scripted(Script {
        refused_registrations: vec![1, 2, 3, 5],
        drop_first_connection: true,
        ..Script::default()
    });
    let base = Duration::from_millis(20);
    let waits = Arc::new(Mutex::new(Vec::new()));
    let config =
        ProjectExecutionConfig::new(fixture.app_bridge.clone(), "provider-under-test", &root)
            .with_reconnect_backoff(base, Duration::from_millis(160), Arc::clone(&waits));
    let _provider = ProjectExecutionProvider::spawn(config).unwrap();

    eventually(|| fixture.bridge.provider_register_times().len() == 6);

    // Doubling per failure; the accepted registration restarts the schedule at the base wait.
    let waits = waits.lock().unwrap().clone();
    assert_eq!(waits, [base, base * 2, base * 4, base, base * 2]);
    // Each attempt arrives no sooner than the wait the provider chose before it.
    let arrivals = fixture.bridge.provider_register_times();
    for (pair, wait) in arrivals.windows(2).zip(&waits) {
        assert!(
            pair[1].duration_since(pair[0]) >= *wait,
            "attempts spaced {:?}, expected at least {wait:?}",
            pair[1].duration_since(pair[0])
        );
    }
}
