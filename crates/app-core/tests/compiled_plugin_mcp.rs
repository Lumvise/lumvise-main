#![cfg(unix)]

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};

use ed25519_dalek::SigningKey;
use lumvise_app_core::{
    AppCore, PluginInvocationRequest, PluginInvocationStatus, ScopedMcpHttpServer,
};
use lumvise_plugin_package::{
    BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility,
    InstalledPlugin, PluginManifest, ProtocolRange, PublisherIdentity, build_package,
    install_verified_package, verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use lumvise_plugin_runtime::{
    DenyAllHostCapabilityBroker, PluginRuntimeConfig, PluginSandbox, PluginSandboxError,
    PluginSandboxRequest, PluginSystem,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TARGET: &str = "app-core-mcp-test";
const EXECUTABLE_PATH: &str = "bin/plugin-runtime-fixture";
const PLUGIN_ID: &str = "plugin.mcp-fixture";
const MANIFEST_EXPORT_ID: &str = "manifest";
const COMPILED_MANIFEST_TOOL: &str = "app_plugin.plugin.mcp-fixture.manifest";
static RUNTIME_FIXTURE_BINARY: OnceLock<Vec<u8>> = OnceLock::new();
const SCOPED_LANE_TOOL: &str = "plugin_mcp_fixture__lane_acquire";

struct CompiledMcpFixture {
    workspace: TempDir,
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

impl CompiledMcpFixture {
    fn build() -> Self {
        Self::with_lane_surface(ExportSurface::ScopedMcpTool {
            scope: "assistant_session".into(),
        })
    }

    fn with_lane_surface(surface: ExportSurface) -> Self {
        let workspace = tempfile::tempdir().expect("compiled MCP workspace");
        let executable_bytes = runtime_fixture_binary();
        let executable_hash = hex::encode(Sha256::digest(&executable_bytes));
        let mut manifest = compiled_mcp_manifest(&executable_hash);
        manifest.exports.last_mut().expect("lane export").surface = surface;
        let signing_key = SigningKey::from_bytes(&[31; 32]);
        let archive = workspace.path().join("plugin.mcp-fixture.lvp");
        build_package(
            &archive,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable_bytes)]),
                signing_key: &signing_key,
            },
        )
        .expect("signed MCP fixture package");
        let verified = verify_package(
            &archive,
            &signing_key.verifying_key(),
            &HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
        )
        .expect("verified MCP fixture package");
        let installed = install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("installed MCP fixture package");
        Self {
            workspace,
            installed,
        }
    }
}

impl Drop for CompiledMcpFixture {
    fn drop(&mut self) {
        make_tree_writable(self.workspace.path());
    }
}

#[test]
fn plugin_mcp_route_lists_installed_plugin_and_reports_unavailable_until_ready() {
    let fixture = CompiledMcpFixture::build();
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    let app = AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system))
        .expect("in-memory database");
    let endpoints = app.plugin_endpoints();
    plugin_system
        .install(&fixture.installed)
        .expect("catalog compiled fixture");

    // Installed plugins are listed immediately; invocations fail until the
    // process is started. This matches the stop/restart contract exercised by
    // `compiled_plugin_restart_keeps_route_and_recovers_invocation`.
    let installed_tools = endpoints.plugin_mcp_tools().expect("installed catalog");
    assert!(contains_tool(&installed_tools, COMPILED_MANIFEST_TOOL));
    assert_tool_unavailable(&endpoints, "is not ready");

    plugin_system
        .start(PLUGIN_ID)
        .expect("start compiled fixture");
    let ready_tools = endpoints
        .plugin_mcp_tools()
        .expect("ready compiled catalog");
    assert!(contains_tool(&ready_tools, COMPILED_MANIFEST_TOOL));
    assert!(!contains_tool(
        &ready_tools,
        "app_plugin.plugin.mcp-fixture.non_mcp"
    ));
    assert!(!contains_tool(
        &ready_tools,
        "app_plugin.plugin.mcp-fixture.scoped_tool"
    ));
    assert_eq!(
        tool_schema(&ready_tools, COMPILED_MANIFEST_TOOL),
        json!({
            "type": "object",
            "properties": {"detail": {"type": "string", "minLength": 2}},
            "additionalProperties": false
        })
    );

    let success = endpoints
        .invoke_plugin_mcp_tool(PluginInvocationRequest {
            tool_name: COMPILED_MANIFEST_TOOL.into(),
            arguments: json!({"detail": "full"}),
        })
        .expect("invoke listed compiled tool");
    assert_eq!(success.status, PluginInvocationStatus::Completed);
    assert_eq!(success.output["capability_id"], MANIFEST_EXPORT_ID);

    plugin_system
        .stop(PLUGIN_ID)
        .expect("stop compiled fixture");
    let stopped_tools = endpoints
        .plugin_mcp_tools()
        .expect("stopped compiled catalog");
    assert!(contains_tool(&stopped_tools, COMPILED_MANIFEST_TOOL));
    assert_tool_unavailable(&endpoints, "is not ready");

    plugin_system
        .uninstall(PLUGIN_ID)
        .expect("detach compiled fixture");
    assert!(
        endpoints
            .plugin_mcp_tools()
            .expect("uninstalled compiled catalog")
            .is_empty()
    );
    assert_tool_not_listed(&endpoints);
}

#[test]
fn global_mcp_tool_preserves_application_failure_and_success_status() {
    let fixture = CompiledMcpFixture::with_lane_surface(ExportSurface::McpTool);
    let plugins = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    let app = AppCore::in_memory_with_plugin_system(Arc::clone(&plugins)).expect("in-memory app");
    plugins
        .install(&fixture.installed)
        .expect("install fixture");
    plugins.start(PLUGIN_ID).expect("start fixture");
    for force_fail in [true, false] {
        let response = app
            .plugin_endpoints()
            .handle_plugin_mcp_json_rpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "app_plugin.plugin.mcp-fixture.lane.acquire",
                    "arguments": {"force_fail": force_fail}}
            }))
            .expect("tool response")
            .expect("response envelope");
        assert_eq!(response["result"]["isError"], force_fail, "{response}");
        assert!(
            response.get("error").is_none(),
            "application failures are MCP tool results"
        );
        if force_fail {
            assert!(
                response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("fixture application failure")
            );
        }
    }
}

#[test]
fn cli_scoped_tool_alias_and_wire_name_share_route_and_validation() {
    let fixture = CompiledMcpFixture::build();
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    let app = Arc::new(
        AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system))
            .expect("in-memory database"),
    );
    plugin_system
        .install(&fixture.installed)
        .expect("catalog compiled fixture");
    plugin_system
        .start(PLUGIN_ID)
        .expect("start compiled fixture");
    let server = ScopedMcpHttpServer::spawn(app).expect("scoped MCP HTTP server");
    let path = "/api/scoped-plugin-mcp/messages/assistant_session/cli-owner/cli-session";

    let listed = scoped_rpc(
        server.base_url(),
        path,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    );
    assert_eq!(listed["result"]["tools"][0]["name"], SCOPED_LANE_TOOL);

    let failed = scoped_rpc(
        server.base_url(),
        path,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": SCOPED_LANE_TOOL, "arguments": { "force_fail": true } }
        }),
    );
    assert_eq!(failed["id"], 2);
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed.get("error").is_none());

    let alias = scoped_rpc(
        server.base_url(),
        path,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "lane.acquire", "arguments": { "force_fail": false } }
        }),
    );
    let wire = scoped_rpc(
        server.base_url(),
        path,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": SCOPED_LANE_TOOL, "arguments": { "force_fail": false } }
        }),
    );
    for (id, response) in [(3, &alias), (4, &wire)] {
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["isError"], false);
        assert!(response.get("error").is_none());
    }
    assert_eq!(
        alias["result"]["content"][0]["text"],
        wire["result"]["content"][0]["text"]
    );
    assert_eq!(
        alias["result"]["content"][0]["text"],
        r#"{"capability_id":"lane.acquire","input":{"force_fail":false,"mcp_owner_id":"cli-owner","plugin_id":"plugin.mcp-fixture","session_id":"cli-session"}}"#
    );

    let epoch_path = format!("{path}/27");
    let epoch_catalog = scoped_rpc(
        server.base_url(),
        &epoch_path,
        json!({"jsonrpc":"2.0","id":30,"method":"tools/list"}),
    );
    assert!(
        epoch_catalog["result"]["tools"][0]["inputSchema"]["properties"]
            .get("session_epoch")
            .is_none()
    );
    let epoch_result = scoped_rpc(
        server.base_url(),
        &epoch_path,
        json!({"jsonrpc":"2.0","id":31,"method":"tools/call",
        "params":{"name":"lane.acquire","arguments":{"force_fail":false}}}),
    );
    let epoch_payload: Value = serde_json::from_str(
        epoch_result["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(epoch_payload["input"]["session_epoch"], 27);
    for (request_id, epoch) in [(32, 28), (33, 27)] {
        let retained = scoped_rpc(
            server.base_url(),
            &format!("{path}/{epoch}"),
            json!({"jsonrpc":"2.0","id":request_id,"method":"tools/call",
            "params":{"name":"lane.acquire","arguments":{"force_fail":false,"session_epoch":999}}}),
        );
        let payload: Value =
            serde_json::from_str(retained["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(
            payload["input"]["session_epoch"], epoch,
            "old route must retain its epoch after a newer route is used"
        );
    }

    let invalid = scoped_rpc(
        server.base_url(),
        path,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "lane.acquire", "arguments": { "force_fail": "not-a-boolean" } }
        }),
    );
    assert_eq!(invalid["id"], 5);
    assert_eq!(invalid["result"]["isError"], true);
    assert!(
        invalid["result"]["content"][0]["text"]
            .as_str()
            .expect("validation error text")
            .contains("boolean")
    );

    let unknown = scoped_rpc(
        server.base_url(),
        path,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "knowledge.find_elements", "arguments": {} }
        }),
    );
    assert_eq!(unknown["id"], 6);
    assert!(
        unknown["error"]["message"]
            .as_str()
            .expect("unknown tool error")
            .contains("unknown scoped MCP tool")
    );
}

fn assert_tool_unavailable(endpoints: &lumvise_app_core::PluginEndpoints<'_>, reason: &str) {
    let error = endpoints
        .invoke_plugin_mcp_tool(PluginInvocationRequest {
            tool_name: COMPILED_MANIFEST_TOOL.into(),
            arguments: json!({"detail": "full"}),
        })
        .expect_err("route should report temporary unavailability");
    assert!(
        error.to_string().contains(reason),
        "expected unavailable error containing {reason:?}, got {error}"
    );
}

fn assert_tool_not_listed(endpoints: &lumvise_app_core::PluginEndpoints<'_>) {
    let error = endpoints
        .invoke_plugin_mcp_tool(PluginInvocationRequest {
            tool_name: COMPILED_MANIFEST_TOOL.into(),
            arguments: json!({"detail": "full"}),
        })
        .expect_err("route should not be discoverable after uninstall");
    assert!(error.to_string().contains("ready compiled plugin tool"));
}

fn compiled_mcp_exports() -> Vec<ExportDescriptor> {
    vec![
        ExportDescriptor {
            id: MANIFEST_EXPORT_ID.into(),
            name: "Compiled fixture manifest".into(),
            description: String::new(),
            surface: ExportSurface::McpTool,
            input_schema: json!({
                "type": "object",
                "properties": {"detail": {"type": "string", "minLength": 2}},
                "additionalProperties": false
            }),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        },
        ExportDescriptor {
            id: "non_mcp".into(),
            name: "Not an MCP export".into(),
            description: String::new(),
            surface: ExportSurface::Command,
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        },
        ExportDescriptor {
            id: "lane.acquire".into(),
            name: "Scoped tool".into(),
            description: String::new(),
            surface: ExportSurface::ScopedMcpTool {
                scope: "assistant_session".into(),
            },
            input_schema: json!({
                "type": "object",
                "properties": {"force_fail": {"type": "boolean"}, "session_epoch": {"type":"integer"}}
            }),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        },
    ]
}

fn compiled_mcp_manifest(executable_sha256: &str) -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "mcp-fixture.release.1".into(),
        },
        plugin_id: PLUGIN_ID.into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange {
            min: u32::from(CURRENT_PROTOCOL_VERSION.major),
            max: u32::from(CURRENT_PROTOCOL_VERSION.major),
        },
        targets: BTreeMap::from([(TARGET.into(), EXECUTABLE_PATH.into())]),
        files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable_sha256.into())]),
        exports: compiled_mcp_exports(),
        host_capabilities: Vec::new(),
    }
}

fn compile_runtime_fixture(workspace: &Path) -> PathBuf {
    let target_dir = workspace.join("target");
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
    target_dir.join("debug/plugin-runtime-fixture")
}

fn runtime_fixture_binary() -> Vec<u8> {
    RUNTIME_FIXTURE_BINARY
        .get_or_init(|| {
            let workspace = tempfile::tempdir().expect("runtime fixture workspace");
            std::fs::read(compile_runtime_fixture(workspace.path()))
                .expect("runtime fixture binary")
        })
        .clone()
}

fn fast_runtime_config() -> PluginRuntimeConfig {
    let mut config =
        PluginRuntimeConfig::default().with_controlled_test_deadline(Duration::from_secs(3));
    config.handshake_timeout = Duration::from_secs(3);
    config.shutdown_grace = Duration::from_secs(1);
    config
}

fn contains_tool(tools: &[lumvise_app_core::PluginMcpTool], name: &str) -> bool {
    tools.iter().any(|tool| tool.tool_name == name)
}

fn tool_schema(tools: &[lumvise_app_core::PluginMcpTool], name: &str) -> serde_json::Value {
    tools
        .iter()
        .find(|tool| tool.tool_name == name)
        .expect("listed compiled tool")
        .mcp_schema()
}

fn scoped_rpc(base_url: &str, path: &str, request: Value) -> Value {
    let body = serde_json::to_vec(&request).expect("encode JSON-RPC request");
    let address = base_url.strip_prefix("http://").expect("local HTTP URL");
    let mut stream = TcpStream::connect(address).expect("connect scoped MCP server");
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write HTTP headers");
    stream.write_all(&body).expect("write JSON-RPC request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read HTTP response");
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response headers");
    let status = std::str::from_utf8(&response[..header_end])
        .expect("UTF-8 headers")
        .split_whitespace()
        .nth(1)
        .expect("HTTP status");
    assert_eq!(status, "200", "scoped MCP response must remain recoverable");
    serde_json::from_slice(&response[header_end + 4..]).expect("JSON-RPC response")
}
fn make_tree_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            make_tree_writable(&path);
        }
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755));
}
