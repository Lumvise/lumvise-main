#![cfg(unix)]
#![cfg(feature = "desktop-app")]

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    process::Command,
    sync::{Arc, Barrier, LazyLock, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ed25519_dalek::SigningKey;
use lumvise_app_core::{
    AcquireResult, ActivationRequest, AppCore, AppCoreDesktopBridge, AppRuntimeCoordinator,
    RuntimeConnection,
};
use lumvise_mcp_core::{
    APP_BRIDGE_PROTOCOL_MAJOR, AppBridgeInvocationRequestV1, AppBridgeInvocationResponseV1,
    AppBridgeInvocationStatusV1,
};
use lumvise_plugin_package::{
    BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility,
    InstalledPlugin, PluginManifest, ProtocolRange, PublisherIdentity, build_package,
    install_verified_package, verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use lumvise_plugin_runtime::{
    DenyAllHostCapabilityBroker, ExportConcurrencyPolicy, PluginInvocationCancellationRequest,
    PluginRuntimeConfig, PluginSandbox, PluginSandboxError, PluginSandboxRequest, PluginSystem,
};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TARGET: &str = "app-core-concurrency-stress";
const EXECUTABLE_PATH: &str = "bin/plugin-runtime-fixture";
const CONCURRENCY_PLUGIN_ID: &str = "mux-concurrency-stress-plugin";
const MCP_TOOL_ID: &str = "mux.barrier";
const CONCURRENT_CLIENTS: usize = 24;
const NORMAL_MCP_CLIENTS: usize = 12;
const NORMAL_MCP_INVOCATION_CAPACITY: u32 = NORMAL_MCP_CLIENTS as u32;
const FAST_REQUESTS_PER_CLIENT: usize = 12;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_FAST_P95_MS: u64 = 500;
const MAX_MCP_WORKLOAD_MS: u64 = 12_000;
static RUNTIME_FIXTURE_BINARY: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let workspace = tempfile::tempdir().expect("runtime fixture workspace");
    let target_dir = workspace.path().join("target");
    let status = Command::new(env!("CARGO"))
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args([
            "build",
            "-p",
            "lumvise-plugin-runtime",
            "--bin",
            "plugin-runtime-fixture",
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("build runtime fixture binary");
    assert!(status.success());
    std::fs::read(target_dir.join("debug/plugin-runtime-fixture")).expect("runtime fixture binary")
});

struct ConcurrencyFixture {
    _workspace: TempDir,
    installed: InstalledPlugin,
}

struct TestSubprocessSandbox;

impl PluginSandbox for TestSubprocessSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        Ok(Command::new(request.executable))
    }
}

impl ConcurrencyFixture {
    fn build() -> Self {
        let workspace = tempfile::tempdir().expect("concurrency fixture workspace");
        let executable = runtime_fixture_binary();
        let executable_hash = hex::encode(Sha256::digest(&executable));
        let manifest = concurrency_manifest(&executable_hash);
        let signing_key = SigningKey::from_bytes(&[29; 32]);
        let archive = workspace.path().join("plugin-concurrency-stress.lvp");
        build_package(
            &archive,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable)]),
                signing_key: &signing_key,
            },
        )
        .expect("build concurrency fixture package");
        let verified = verify_package(
            &archive,
            &signing_key.verifying_key(),
            &HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
        )
        .expect("verify fixture package");
        let installed = install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("install fixture package");
        Self {
            _workspace: workspace,
            installed,
        }
    }
}

struct TestBridge {
    _runtime_root: TempDir,
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

    fn endpoint(&self) -> BridgeEndpoint {
        BridgeEndpoint {
            base_url: self.connection.app_bridge_base_url.clone(),
            credential: self.connection.app_bridge_credential.clone(),
        }
    }
}

#[test]
fn concurrent_mcp_and_health_clients_remain_responsive() {
    let fixture = ConcurrencyFixture::build();
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        concurrent_runtime_config(),
        Arc::new(DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    plugin_system
        .install(&fixture.installed)
        .expect("install concurrency fixture");
    let app = Arc::new(
        AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system)).expect("in-memory db"),
    );

    let bridge = TestBridge::new(Arc::clone(&app));
    let endpoint = bridge.endpoint();
    wait_for_ready_plugin_surface(&endpoint);

    let start_barrier = Arc::new(Barrier::new(CONCURRENT_CLIENTS));
    let mut handles = Vec::with_capacity(CONCURRENT_CLIENTS);

    for worker in 0..NORMAL_MCP_CLIENTS {
        let endpoint = bridge.endpoint();
        let start_barrier = Arc::clone(&start_barrier);
        handles.push(thread::spawn(move || -> Result<Vec<Duration>, String> {
            start_barrier.wait();
            let started = Instant::now();
            invoke_mcp_tool(
                &endpoint,
                format!("mcp-{worker}"),
                format!("pair-{}", worker / 2),
            )?;
            Ok(vec![started.elapsed()])
        }));
    }

    for _worker in NORMAL_MCP_CLIENTS..CONCURRENT_CLIENTS {
        let endpoint = bridge.endpoint();
        let start_barrier = Arc::clone(&start_barrier);
        handles.push(thread::spawn(move || -> Result<Vec<Duration>, String> {
            start_barrier.wait();
            let mut latencies = Vec::with_capacity(FAST_REQUESTS_PER_CLIENT);
            for _ in 0..FAST_REQUESTS_PER_CLIENT {
                let started = Instant::now();
                get_health(&endpoint)?;
                latencies.push(started.elapsed());
            }
            Ok(latencies)
        }));
    }

    let started = Instant::now();
    let mut fast_latencies =
        Vec::with_capacity((CONCURRENT_CLIENTS - NORMAL_MCP_CLIENTS) * FAST_REQUESTS_PER_CLIENT);
    let mut errors = Vec::new();
    for (worker, handle) in handles.into_iter().enumerate() {
        match handle.join() {
            Ok(Ok(latencies)) if worker >= NORMAL_MCP_CLIENTS => fast_latencies.extend(latencies),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => errors.push(error),
            Err(_) => errors.push(format!("client {worker} panicked")),
        }
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(MAX_MCP_WORKLOAD_MS),
        "concurrent workload exceeded deadline: {elapsed:?}"
    );
    assert!(
        errors.is_empty(),
        "{} concurrent requests failed: {errors:?}",
        errors.len()
    );
    assert!(
        !fast_latencies.is_empty(),
        "no GET /health requests completed"
    );
    let fast_p95 = p95(&mut fast_latencies);
    assert!(
        fast_p95 < Duration::from_millis(MAX_FAST_P95_MS),
        "p95 GET /health latency exceeded async-server budget: {fast_p95:?}"
    );
}

#[test]
fn immediate_client_eof_cancels_exact_invoke_v1_without_cancelling_sibling() {
    let fixture = ConcurrencyFixture::build();
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        concurrent_runtime_config(),
        Arc::new(DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    plugin_system
        .install(&fixture.installed)
        .expect("install concurrency fixture");
    let app = Arc::new(
        AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system)).expect("in-memory db"),
    );
    let bridge = TestBridge::new(Arc::clone(&app));
    let endpoint = bridge.endpoint();
    wait_for_ready_plugin_surface(&endpoint);

    let (sibling_sender, sibling_receiver) = mpsc::channel();
    let sibling_endpoint = bridge.endpoint();
    let sibling = thread::spawn(move || {
        let result = invoke_mcp_tool_response(
            &sibling_endpoint,
            "eof-sibling-request",
            "eof-sibling-owner",
            "eof-sibling-session",
            "eof-sibling-scope",
            "eof-isolation-barrier",
        );
        let _ = sibling_sender.send(result);
    });
    wait_for_plugin_dispatch(
        &plugin_system,
        true,
        "sibling invocation to enter mux barrier",
    );

    let eof_payload = mcp_invocation_payload(
        "eof-cancelled-request",
        "eof-cancelled-owner",
        "eof-cancelled-session",
        "eof-cancelled-scope",
        "eof-isolation-barrier",
    );
    let mut eof_client =
        send_invoke_request_then_close_write(&endpoint, &eof_payload).expect("send EOF request");

    let sibling_response = sibling_receiver
        .recv_timeout(REQUEST_TIMEOUT)
        .expect("sibling invocation exceeded cancellation deadline")
        .expect("sibling invoke-v1 response");
    sibling.join().expect("sibling client thread");
    assert_eq!(sibling_response.request_id, "eof-sibling-request");
    assert_ne!(
        sibling_response.status,
        AppBridgeInvocationStatusV1::Cancelled as i32,
        "EOF for tuple A must not cancel sibling tuple B"
    );

    let mut eof_response = Vec::new();
    eof_client
        .read_to_end(&mut eof_response)
        .expect("read EOF client response");
    assert!(
        eof_response.is_empty(),
        "cancelled EOF request must not receive an HTTP response: {eof_response:?}"
    );
    wait_for_plugin_dispatch(&plugin_system, false, "EOF invocation terminal cleanup");
    assert!(
        !plugin_system
            .cancel_controlled(&PluginInvocationCancellationRequest {
                plugin_id: Some(CONCURRENCY_PLUGIN_ID.into()),
                request_id: "eof-cancelled-request".into(),
                owner_id: "eof-cancelled-owner".into(),
                session_id: Some("eof-cancelled-session".into()),
                scope_id: Some("eof-cancelled-scope".into()),
            })
            .expect("query EOF tuple after terminal cleanup"),
        "EOF tuple A remained registered after terminal cleanup"
    );
}

fn p95(latencies: &mut [Duration]) -> Duration {
    latencies.sort_unstable();
    let index = (latencies.len() * 95).div_ceil(100) - 1;
    latencies[index]
}

fn wait_for_ready_plugin_surface(endpoint: &BridgeEndpoint) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if get_json(endpoint, "/api/mcp/plugins/surface").is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("concurrency fixture did not publish surface in time");
}

fn wait_for_plugin_dispatch(plugin_system: &PluginSystem, expected: bool, expectation: &str) {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    while Instant::now() < deadline {
        let snapshot = plugin_system
            .invocation_admission_snapshot(CONCURRENCY_PLUGIN_ID)
            .expect("read plugin admission snapshot");
        if snapshot.executing == expected && (expected || snapshot.queued == 0) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let snapshot = plugin_system
        .invocation_admission_snapshot(CONCURRENCY_PLUGIN_ID)
        .expect("read plugin admission snapshot after timeout");
    panic!("timed out waiting for {expectation}; final admission snapshot: {snapshot:?}");
}

fn runtime_fixture_binary() -> Vec<u8> {
    RUNTIME_FIXTURE_BINARY.clone()
}

fn concurrency_manifest(executable_sha256: &str) -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "concurrency-stress.release.1".into(),
        },
        plugin_id: CONCURRENCY_PLUGIN_ID.into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange {
            min: u32::from(CURRENT_PROTOCOL_VERSION.major),
            max: u32::from(CURRENT_PROTOCOL_VERSION.major),
        },
        targets: BTreeMap::from([(TARGET.into(), EXECUTABLE_PATH.into())]),
        files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable_sha256.into())]),
        exports: exports(),
        host_capabilities: Vec::new(),
    }
}

fn exports() -> Vec<ExportDescriptor> {
    vec![ExportDescriptor {
        id: MCP_TOOL_ID.into(),
        name: "Concurrent MCP barrier fixture".into(),
        description: String::new(),
        surface: ExportSurface::McpTool,
        input_schema: json!({
            "type": "object",
            "required": ["barrier"],
            "properties": {"barrier": {"type": "string"}},
            "additionalProperties": false
        }),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    }]
}

fn concurrent_runtime_config() -> PluginRuntimeConfig {
    let mut config =
        PluginRuntimeConfig::default().with_controlled_test_deadline(Duration::from_secs(2));
    config.handshake_timeout = Duration::from_millis(500);
    config.shutdown_grace = Duration::from_millis(200);
    config.max_concurrent_invocations_per_plugin = NORMAL_MCP_INVOCATION_CAPACITY;
    config.export_concurrency.set_policy(
        MCP_TOOL_ID.into(),
        ExportConcurrencyPolicy::Parallel(NORMAL_MCP_INVOCATION_CAPACITY),
    );
    config
}

fn invoke_mcp_tool(
    endpoint: &BridgeEndpoint,
    request_id: String,
    barrier: String,
) -> Result<u16, String> {
    let response = invoke_mcp_tool_response(
        endpoint,
        &request_id,
        "concurrency-owner",
        "concurrency-session",
        "concurrency-scope",
        &barrier,
    )?;
    if response.status != AppBridgeInvocationStatusV1::Completed as i32 {
        return Err(format!(
            "invoke status was {}: {}",
            response.status, response.message
        ));
    }
    Ok(200)
}

fn invoke_mcp_tool_response(
    endpoint: &BridgeEndpoint,
    request_id: &str,
    owner_id: &str,
    session_id: &str,
    scope_id: &str,
    barrier: &str,
) -> Result<AppBridgeInvocationResponseV1, String> {
    let payload = mcp_invocation_payload(request_id, owner_id, session_id, scope_id, barrier);
    let (status, body) = send_request(
        endpoint,
        "POST",
        "/api/mcp/plugins/invoke-v1",
        Some("application/protobuf"),
        &payload,
    )?;
    if status != 200 {
        return Err(format!("mcp invocation returned HTTP status {status}"));
    }
    AppBridgeInvocationResponseV1::decode(body.as_slice())
        .map_err(|error| format!("decode invoke response: {error}"))
}

fn mcp_invocation_payload(
    request_id: &str,
    owner_id: &str,
    session_id: &str,
    scope_id: &str,
    barrier: &str,
) -> Vec<u8> {
    let deadline_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_millis() as u64
        + 5_000;
    AppBridgeInvocationRequestV1 {
        protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
        request_id: request_id.into(),
        owner_id: owner_id.into(),
        session_id: session_id.into(),
        scope_id: Some(scope_id.into()),
        deadline_unix_ms,
        plugin_id: CONCURRENCY_PLUGIN_ID.into(),
        capability_id: MCP_TOOL_ID.into(),
        input_json: serde_json::to_vec(&json!({"barrier": barrier}))
            .expect("serialize invocation input"),
    }
    .encode_to_vec()
}

fn get_json(endpoint: &BridgeEndpoint, path: &str) -> Result<Value, String> {
    let (status, body) = send_request(endpoint, "GET", path, None, &[])?;
    if status != 200 {
        return Err(format!("GET {path} returned {status}"));
    }
    serde_json::from_slice(&body).map_err(|error| error.to_string())
}

fn get_health(endpoint: &BridgeEndpoint) -> Result<(), String> {
    let health = get_json(endpoint, "/health")?;
    if health["ok"].as_bool() != Some(true) {
        return Err("GET /health did not report a healthy daemon".to_owned());
    }
    Ok(())
}

fn send_invoke_request_then_close_write(
    endpoint: &BridgeEndpoint,
    payload: &[u8],
) -> Result<TcpStream, String> {
    let authority = endpoint
        .base_url
        .strip_prefix("http://")
        .ok_or("base URL was not HTTP")?;
    let address: SocketAddr = authority
        .parse()
        .map_err(|error| format!("invalid HTTP authority `{authority}`: {error}"))?;
    let mut stream =
        TcpStream::connect_timeout(&address, REQUEST_TIMEOUT).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "POST /api/mcp/plugins/invoke-v1 HTTP/1.1\r\nHost: {authority}\r\n\
         Authorization: Bearer {}\r\nConnection: close\r\nContent-Type: application/protobuf\r\n\
         Content-Length: {}\r\n\r\n",
        endpoint.credential,
        payload.len()
    )
    .map_err(|error| error.to_string())?;
    stream
        .write_all(payload)
        .map_err(|error| error.to_string())?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| error.to_string())?;
    Ok(stream)
}

fn send_request(
    endpoint: &BridgeEndpoint,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    let authority = endpoint
        .base_url
        .strip_prefix("http://")
        .ok_or("base URL was not HTTP")?;
    let address: SocketAddr = authority
        .parse()
        .map_err(|error| format!("invalid HTTP authority `{authority}`: {error}"))?;
    let mut stream =
        TcpStream::connect_timeout(&address, REQUEST_TIMEOUT).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(REQUEST_TIMEOUT))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\n\
         Authorization: Bearer {}\r\nConnection: close\r\n",
        endpoint.credential
    )
    .map_err(|error| error.to_string())?;
    if method == "POST" {
        write!(
            stream,
            "Content-Type: {}\r\nContent-Length: {}\r\n",
            content_type.unwrap_or("application/json"),
            body.len()
        )
        .map_err(|error| error.to_string())?;
    }
    stream
        .write_all(b"\r\n")
        .map_err(|error| error.to_string())?;
    if !body.is_empty() {
        stream.write_all(body).map_err(|error| error.to_string())?;
    }
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("missing HTTP response body")?;
    let head = std::str::from_utf8(&response[..split]).map_err(|error| error.to_string())?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or("invalid HTTP status")?;
    Ok((status, response[split + 4..].to_vec()))
}
