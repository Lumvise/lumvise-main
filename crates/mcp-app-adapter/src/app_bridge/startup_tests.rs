use super::*;
use crate::{McpAppConfig, open_server};
use lumvise_app_core::{
    AcquireResult, ActivationRequest, OwnerLease, RuntimeCoordinatorError, RuntimeLauncher,
};
use lumvise_mcp_core::LumviseMcpServer;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

struct FakeStartupLauncher(Sender<ActivationRequest>);

impl RuntimeLauncher for FakeStartupLauncher {
    fn launch(&self, request: ActivationRequest) -> Result<(), RuntimeCoordinatorError> {
        let _ = self.0.send(request);
        Ok(())
    }
}

struct FakeCatalogBridge {
    base_url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FakeCatalogBridge {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::clone(&stop);
        let worker = thread::spawn(move || Self::serve(listener, shutdown));
        Self {
            base_url,
            stop,
            worker: Some(worker),
        }
    }

    fn serve(listener: TcpListener, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Acquire) {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut header = String::new();
            while reader.read_line(&mut header).unwrap() > 0 && !header.ends_with("\r\n\r\n") {}
            let body = json!({"generation":"ready-fixture","capabilities":[{
                "plugin_id":"plugin.example","capability_id":"run",
                "dynamic_tool_name":"app_plugin.plugin.example.run"
            }]})
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    }
}

impl Drop for FakeCatalogBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

struct StartingAppFixture {
    server: Arc<LumviseMcpServer>,
    owner: OwnerLease,
    launches: Receiver<ActivationRequest>,
    _root: tempfile::TempDir,
}

impl StartingAppFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (launched, launches) = channel();
        let coordinator = Arc::new(AppRuntimeCoordinator::new(
            root.path(),
            FakeStartupLauncher(launched),
        ));
        let AcquireResult::Owner(owner) = coordinator
            .acquire_or_forward(ActivationRequest::default())
            .unwrap()
        else {
            panic!("expected isolated starting owner");
        };
        let config = AppBridgeConfig {
            discovery_path: root.path().join(RUNTIME_DISCOVERY_FILE),
            coordinator,
            terminal: Arc::new(AtomicBool::new(false)),
        };
        let server = Arc::new(open_server(McpAppConfig::from_app_bridge(config)));
        Self {
            server,
            owner,
            launches,
            _root: root,
        }
    }
}

#[test]
fn repeated_discovery_starts_only_one_background_connection_attempt() {
    let fixture = StartingAppFixture::new();
    rpc(&fixture.server, "tools/list", json!({}));
    let launch = fixture
        .launches
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(
        launch.background,
        "MCP starts must never foreground an existing generation"
    );
    for _ in 0..3 {
        let reply = rpc(
            &fixture.server,
            "tools/call",
            json!({"name":"discover_app_plugins","arguments":{}}),
        );
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("initializing")
        );
    }
    assert!(fixture.launches.try_recv().is_err());
}

#[test]
fn mcp_status_does_not_relaunch_an_explicitly_quit_generation() {
    let mut fixture = StartingAppFixture::new();
    fixture.owner.mark_ready("http://127.0.0.1:1").unwrap();
    rpc(
        &fixture.server,
        "tools/call",
        json!({"name":"app_bridge_status","arguments":{}}),
    );
    fixture
        .owner
        .begin_quit(lumvise_app_core::QuitRequest::default())
        .unwrap();
    let reply = rpc(
        &fixture.server,
        "tools/call",
        json!({"name":"app_bridge_status","arguments":{}}),
    );
    let status: Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["state"], "unavailable");
    assert!(status["error"].as_str().unwrap().contains("quitting"));
    assert!(fixture.launches.try_recv().is_err());
}

fn rpc(server: &LumviseMcpServer, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    serde_json::from_str(
        &server
            .handle_json_line(&request.to_string())
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn mcp_base_tools_respond_before_app_initialization_and_discover_ready_plugins_later() {
    let mut fixture = StartingAppFixture::new();
    let server = Arc::clone(&fixture.server);
    let (sender, receiver) = channel();
    let request =
        thread::spawn(move || sender.send(rpc(&server, "tools/list", json!({}))).unwrap());
    let early = receiver.recv_timeout(Duration::from_millis(300));
    let bridge = FakeCatalogBridge::start();
    fixture.owner.mark_ready(bridge.base_url.clone()).unwrap();
    request.join().unwrap();
    let tools = early.expect("MCP tools/list must not wait for database or plugin initialization");
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 3);
    let ready = rpc(&fixture.server, "tools/list", json!({}));
    assert!(
        ready["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "app_plugin.plugin.example.run")
    );
}

#[test]
fn mcp_status_does_not_wait_for_app_initialization() {
    let mut fixture = StartingAppFixture::new();
    let server = Arc::clone(&fixture.server);
    let (sender, receiver) = channel();
    let request = thread::spawn(move || {
        sender
            .send(rpc(
                &server,
                "tools/call",
                json!({"name":"app_bridge_status","arguments":{}}),
            ))
            .unwrap()
    });
    let early = receiver.recv_timeout(Duration::from_millis(300));
    let bridge = FakeCatalogBridge::start();
    fixture.owner.mark_ready(bridge.base_url.clone()).unwrap();
    request.join().unwrap();
    let reply = early.expect("MCP status must not wait for database initialization");
    let status: Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["linked"], false);
    assert_eq!(status["state"], "initializing");
}
