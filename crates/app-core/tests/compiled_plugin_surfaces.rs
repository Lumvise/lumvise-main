#![cfg(unix)]
#![cfg(feature = "desktop-app")]

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use ed25519_dalek::SigningKey;
use lumvise_app_core::{
    AcquireResult, ActivationRequest, AppCore, AppCoreDesktopBridge, AppRuntimeCoordinator,
    RuntimeConnection,
};
use lumvise_frontend_core::{RendererViewSource, RendererViewSurface};
use lumvise_plugin_package::{
    BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCapabilityRequirement,
    HostCompatibility, HttpMethod, HttpStreamMode, InstalledPlugin, PluginManifest, ProtocolRange,
    PublisherIdentity, SseStreamPolicy, ViewSurface, build_package, install_verified_package,
    verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginInvocationClass,
    PluginInvocationContext, PluginRuntimeConfig, PluginRuntimeError, PluginSandbox,
    PluginSandboxError, PluginSandboxRequest, PluginSystem,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TARGET: &str = "app-core-surfaces-test";
const EXECUTABLE_PATH: &str = "bin/plugin-runtime-fixture";
const PLUGIN_ID: &str = "plugin.surface-fixture";
const HTTP_EXPORT_ID: &str = "manifest";
const VIEW_ID: &str = "compiled_fixture_panel";
const VIEW_ASSET: &str = "views/compiled-panel.html";
const VIEW_STYLE: &str = "views/compiled-panel.css";
const VIEW_SCRIPT: &str = "views/compiled-panel.js";
const VIEW_HTML_BYTES: &[u8] = b"<!doctype html><title>Compiled panel</title>";
const VIEW_STYLE_BYTES: &[u8] = b"body { color: white; }";
const VIEW_SCRIPT_BYTES: &[u8] = b"document.body.dataset.ready = 'true';";
static RUNTIME_FIXTURE_BINARY: OnceLock<Vec<u8>> = OnceLock::new();

struct CompiledSurfaceFixture {
    workspace: TempDir,
    installed: InstalledPlugin,
}

struct TestSubprocessSandbox;
struct AllowViewBroker;

impl HostCapabilityBroker for AllowViewBroker {
    fn invoke(&self, request: HostCapabilityRequest) -> Result<Value, HostCapabilityError> {
        Ok(json!({"api_id": request.capability_id, "input": request.input}))
    }
}

impl PluginSandbox for TestSubprocessSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        Ok(Command::new(request.executable))
    }
}

impl CompiledSurfaceFixture {
    fn build(valid_output_schema: bool) -> Self {
        Self::build_surface(valid_output_schema, ViewSurface::DashboardPanel)
    }

    fn build_surface(valid_output_schema: bool, surface: ViewSurface) -> Self {
        let workspace = tempfile::tempdir().expect("compiled surface workspace");
        let executable_bytes = runtime_fixture_binary_bytes();
        let executable_hash = hex::encode(Sha256::digest(&executable_bytes));
        let mut manifest = fixture_manifest(PLUGIN_ID, &executable_hash);
        manifest.host_capabilities.push(HostCapabilityRequirement {
            id: "fixture.read".into(),
            version: "^1.0".into(),
        });
        manifest.files.extend(
            test_view_assets()
                .into_iter()
                .map(|(path, bytes)| (path.into(), hex::encode(Sha256::digest(bytes)))),
        );
        manifest.exports = compiled_surface_exports(valid_output_schema);
        for export in &mut manifest.exports {
            if let ExportSurface::View {
                surface: declared, ..
            } = &mut export.surface
            {
                *declared = surface;
            }
        }
        let package_executable = manifest.targets[TARGET].clone();
        let signing_key = SigningKey::from_bytes(&[47; 32]);
        let archive = workspace.path().join("plugin.surface-fixture.lvp");
        let mut files = BTreeMap::from([(package_executable, executable_bytes)]);
        files.extend(
            test_view_assets()
                .into_iter()
                .map(|(path, bytes)| (path.into(), bytes.to_vec())),
        );
        build_package(
            &archive,
            BuildPackageRequest {
                manifest,
                files,
                signing_key: &signing_key,
            },
        )
        .expect("signed surface package");
        let verified = verify_package(
            &archive,
            &signing_key.verifying_key(),
            &HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
        )
        .expect("verified surface package");
        let installed = install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("installed surface package");
        Self {
            workspace,
            installed,
        }
    }
}

#[test]
fn native_view_assets_follow_plugin_readiness_and_survive_reactivation() {
    use lumvise_frontend_core::DesktopSettingsBridge;
    let fixture = CompiledSurfaceFixture::build_surface(true, ViewSurface::NativeWindow);
    let system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(AllowViewBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    let app = Arc::new(AppCore::in_memory_with_plugin_system(Arc::clone(&system)).unwrap());
    let server = TestBridge::new(Arc::clone(&app));
    system.install(&fixture.installed).unwrap();
    assert!(
        server
            ._bridge
            .read_plugin_view_asset(PLUGIN_ID, VIEW_ID, "")
            .is_err()
    );
    for _ in 0..2 {
        system.start(PLUGIN_ID).unwrap();
        assert_eq!(
            app.plugin_endpoints().compiled_plugin_views().unwrap()[0].surface,
            RendererViewSurface::NativeWindow
        );
        assert_eq!(
            server
                ._bridge
                .read_plugin_view_asset(PLUGIN_ID, VIEW_ID, "")
                .unwrap()
                .bytes,
            VIEW_HTML_BYTES
        );
        assert!(
            server
                ._bridge
                .read_plugin_view_asset(PLUGIN_ID, VIEW_ID, "../outside.js")
                .is_err()
        );
        system.stop(PLUGIN_ID).unwrap();
        assert!(
            app.plugin_endpoints()
                .compiled_plugin_views()
                .unwrap()
                .is_empty()
        );
        assert!(
            server
                ._bridge
                .read_plugin_view_asset(PLUGIN_ID, VIEW_ID, "compiled-panel.js")
                .is_err()
        );
    }
}

impl Drop for CompiledSurfaceFixture {
    fn drop(&mut self) {
        make_tree_writable(self.workspace.path());
    }
}

struct SseSurfaceFixture {
    workspace: TempDir,
    installed: InstalledPlugin,
}

impl SseSurfaceFixture {
    fn build(plugin_id: &str) -> Self {
        let workspace = tempfile::tempdir().expect("SSE fixture workspace");
        let executable_bytes = runtime_fixture_binary_bytes();
        let executable_hash = hex::encode(Sha256::digest(&executable_bytes));
        let mut manifest = fixture_manifest(plugin_id, &executable_hash);
        manifest.exports = vec![sse_export()];
        manifest.host_capabilities.clear();
        let executable_path = manifest.targets[TARGET].clone();
        let signing_key = SigningKey::from_bytes(&[53; 32]);
        let archive = workspace.path().join(format!("{plugin_id}.lvp"));
        build_package(
            &archive,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([(executable_path, executable_bytes)]),
                signing_key: &signing_key,
            },
        )
        .expect("signed SSE fixture package");
        let verified = verify_package(
            &archive,
            &signing_key.verifying_key(),
            &HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
        )
        .expect("verified SSE fixture package");
        let installed = install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("installed SSE fixture package");
        Self {
            workspace,
            installed,
        }
    }
}

impl Drop for SseSurfaceFixture {
    fn drop(&mut self) {
        make_tree_writable(self.workspace.path());
    }
}

#[test]
fn ready_compiled_http_and_view_surfaces_attach_and_detach_without_static_bypass() {
    let fixture = CompiledSurfaceFixture::build(true);
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(AllowViewBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    let app = Arc::new(
        AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system))
            .expect("in-memory database"),
    );
    plugin_system
        .install(&fixture.installed)
        .expect("catalog compiled surfaces");
    assert!(
        app.plugin_endpoints()
            .compiled_plugin_views()
            .expect("stopped view catalog")
            .is_empty()
    );

    plugin_system.start(PLUGIN_ID).expect("start surfaces");
    let views = app
        .plugin_endpoints()
        .compiled_plugin_views()
        .expect("ready view catalog");
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].view_id, VIEW_ID);
    assert_eq!(views[0].surface, RendererViewSurface::DashboardPanel);
    assert_eq!(
        views[0].source,
        RendererViewSource::Compiled {
            asset_url: format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/"),
            asset_path: VIEW_ASSET.into(),
            content_security_policy: "default-src 'none'; script-src 'self'".into(),
            allowed_host_apis: vec!["fixture.read".into()],
        }
    );
    let frontend = app
        .frontend()
        .frontend_status()
        .expect("merged frontend views");
    assert_eq!(
        frontend
            .views
            .get(VIEW_ID)
            .map(|view| view.plugin_id.as_str()),
        Some(PLUGIN_ID)
    );
    assert!(frontend.views.get("stale_plugin_view").is_none());

    let server = TestBridge::new(Arc::clone(&app));
    assert!(
        lumvise_frontend_core::DesktopSettingsBridge::read_plugin_view_asset(
            &server._bridge,
            PLUGIN_ID,
            VIEW_ID,
            ""
        )
        .unwrap_err()
        .contains("expected signed native_window surface")
    );
    let success = send_request(
        server.base_url(),
        "POST",
        "/api/compiled/items/item-7?detail=full",
        br#"{"requested":true}"#,
    );
    assert_eq!(success.status, 200);
    assert_eq!(success.content_type, "application/json");
    assert_eq!(success.json()["capability_id"], HTTP_EXPORT_ID);
    assert_eq!(
        success.json()["input"]["path_parameters"]["item_id"],
        "item-7"
    );

    let wrong_method = send_request(server.base_url(), "GET", "/api/compiled/items/item-7", &[]);
    assert_eq!(wrong_method.status, 405);
    let formerly_limited = send_request(
        server.base_url(),
        "POST",
        "/api/compiled/items/item-7",
        format!(r#"{{"payload":"{}"}}"#, "x".repeat(129)).as_bytes(),
    );
    assert_eq!(formerly_limited.status, 200);
    let old_static_bypass = send_request(server.base_url(), "GET", "/api/legacy/manifest", &[]);
    assert_eq!(old_static_bypass.status, 404);
    let asset = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/"),
        &[],
    );
    assert_eq!(asset.status, 200);
    assert_eq!(asset.body, view_asset(VIEW_ASSET));
    let css = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/compiled-panel.css"),
        &[],
    );
    assert_eq!(css.body, view_asset(VIEW_STYLE));
    assert_eq!(css.content_type, "text/css; charset=utf-8");
    let script = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/compiled-panel.js"),
        &[],
    );
    assert_eq!(script.body, view_asset(VIEW_SCRIPT));
    assert_eq!(script.content_type, "text/javascript; charset=utf-8");
    let unsigned = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/not-signed.js"),
        &[],
    );
    assert_eq!(unsigned.status, 409);
    replace_view_asset_with_symlink(
        fixture.installed.root(),
        VIEW_STYLE,
        fixture.workspace.path(),
    );
    let symlinked = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/compiled-panel.css"),
        &[],
    );
    assert_eq!(symlinked.status, 409);
    assert_eq!(
        asset
            .headers
            .get("Content-Security-Policy")
            .map(String::as_str),
        Some("default-src 'none'; script-src 'self'")
    );
    let allowed = plugin_system
        .invoke_view_host_api_controlled(
            PLUGIN_ID,
            VIEW_ID,
            "fixture.read",
            json!({"artifact_id": "a-1"}),
            PluginInvocationContext::new(
                "view-test-allowed",
                "view-test-owner",
                PluginInvocationClass::Foreground,
                Instant::now() + Duration::from_secs(1),
            ),
        )
        .expect("signed View Host API");
    assert_eq!(allowed["api_id"], "fixture.read");
    let blocked = plugin_system
        .invoke_view_host_api_controlled(
            PLUGIN_ID,
            VIEW_ID,
            "network.fetch",
            json!({}),
            PluginInvocationContext::new(
                "view-test-blocked",
                "view-test-owner",
                PluginInvocationClass::Foreground,
                Instant::now() + Duration::from_secs(1),
            ),
        )
        .expect_err("unsigned View Host API denied");
    assert!(matches!(
        blocked.runtime_error(),
        PluginRuntimeError::ViewHostApiDenied { .. }
    ));
    let traversal = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/%2e%2e/secret"),
        &[],
    );
    assert_eq!(traversal.status, 404);
    replace_view_asset(fixture.installed.root().join(VIEW_ASSET));
    let tampered = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/"),
        &[],
    );
    assert_eq!(tampered.status, 409);

    let invalid_signed_input = send_request(
        server.base_url(),
        "POST",
        "/api/compiled/items/item-8",
        br#"{"requested":true}"#,
    );
    assert_eq!(invalid_signed_input.status, 400);
    assert_eq!(
        app.plugin_endpoints()
            .compiled_plugin_views()
            .expect("degraded views")
            .len(),
        1
    );

    plugin_system.stop(PLUGIN_ID).expect("stop surfaces");
    assert!(
        app.plugin_endpoints()
            .compiled_plugin_views()
            .expect("stopped views")
            .is_empty()
    );
    let detached = send_request(
        server.base_url(),
        "POST",
        "/api/compiled/items/item-7",
        br#"{"requested":true}"#,
    );
    assert_ne!(
        detached.status, 404,
        "cataloged route must survive process restart"
    );
    let static_fallback = send_request(server.base_url(), "GET", "/api/legacy/manifest", &[]);
    assert_eq!(static_fallback.status, 404);
    let frontend = app
        .frontend()
        .frontend_status()
        .expect("stopped frontend views");
    assert!(frontend.views.get("stale_plugin_view").is_none());
    let stopped_asset = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/"),
        &[],
    );
    assert_eq!(stopped_asset.status, 404);
    wait_for_plugin_active(&plugin_system, PLUGIN_ID);
    assert_eq!(
        app.plugin_endpoints()
            .compiled_plugin_views()
            .expect("re-enabled views")
            .len(),
        1
    );
    plugin_system
        .stop(PLUGIN_ID)
        .expect("stop re-enabled surfaces");
    plugin_system
        .uninstall(PLUGIN_ID)
        .expect("uninstall surfaces");
    let uninstalled_asset = send_request(
        server.base_url(),
        "GET",
        &format!("/api/plugin-views/{PLUGIN_ID}/{VIEW_ID}/assets/"),
        &[],
    );
    assert_eq!(uninstalled_asset.status, 404);
}

#[test]
fn invalid_compiled_http_output_degrades_plugin_and_removes_view() {
    let fixture = CompiledSurfaceFixture::build(false);
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(AllowViewBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    let app = Arc::new(
        AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system))
            .expect("in-memory database"),
    );
    plugin_system
        .install(&fixture.installed)
        .expect("catalog invalid-output package");
    plugin_system
        .start(PLUGIN_ID)
        .expect("start invalid-output package");
    let server = TestBridge::new(Arc::clone(&app));

    let response = send_request(
        server.base_url(),
        "POST",
        "/api/compiled/items/item-7",
        br#"{"requested":true}"#,
    );

    assert_eq!(response.status, 502);
    assert!(!plugin_system.is_active(PLUGIN_ID).expect("runtime state"));
    assert!(
        app.plugin_endpoints()
            .compiled_plugin_views()
            .expect("degraded views")
            .is_empty()
    );
    let no_static_fallback = send_request(server.base_url(), "GET", "/api/legacy/manifest", &[]);
    assert_eq!(no_static_fallback.status, 404);
    let stable_route = send_request(
        server.base_url(),
        "POST",
        "/api/compiled/items/item-7",
        br#"{"requested":true}"#,
    );
    assert_ne!(
        stable_route.status, 404,
        "degraded route must remain registered"
    );
}

fn wait_for_plugin_active(system: &PluginSystem, plugin_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if system.is_active(plugin_id).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("plugin `{plugin_id}` did not restart within 3 seconds");
}

#[test]
fn compiled_sse_polls_three_frames_without_content_length() {
    let fixture = SseSurfaceFixture::build("sse-three");
    let (plugin_system, app) = started_sse_app(&fixture);
    let server = TestBridge::new(app);

    let response = send_request(server.base_url(), "GET", "/api/fixture/events", &[]);

    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "text/event-stream");
    assert!(!response.headers.contains_key("Content-Length"));
    let body = String::from_utf8(response.body).expect("UTF-8 SSE body");
    assert_eq!(body.matches("event: fixture.changed").count(), 3);
    assert!(body.contains("id: event-1"));
    assert!(body.contains("id: event-2"));
    assert!(body.contains("id: event-3"));
    plugin_system.stop("sse-three").expect("stop SSE fixture");
}

#[test]
fn compiled_sse_malformed_frame_and_crash_emit_terminal_error() {
    for plugin_id in ["sse-malformed", "sse-crash"] {
        let fixture = SseSurfaceFixture::build(plugin_id);
        let (plugin_system, app) = started_sse_app(&fixture);
        let server = TestBridge::new(app);

        let response = send_request(server.base_url(), "GET", "/api/fixture/events", &[]);

        let body = String::from_utf8(response.body).expect("UTF-8 SSE error");
        assert!(body.contains("event: error"), "{plugin_id}: {body}");
        assert!(!plugin_system.is_active(plugin_id).expect("runtime state"));
    }
}

#[test]
fn compiled_sse_stops_on_plugin_stop_and_uninstall() {
    let fixture = SseSurfaceFixture::build("sse-never");
    let (plugin_system, app) = started_sse_app(&fixture);
    let server = TestBridge::new(app);
    let mut stream = open_stream(server.base_url(), "/api/fixture/events");
    let headers = read_until_headers(&mut stream);
    assert!(headers.contains("Content-Type: text/event-stream"));

    plugin_system
        .stop("sse-never")
        .expect("stop live SSE plugin");
    let mut tail = Vec::new();
    stream
        .read_to_end(&mut tail)
        .expect("stream closes after stop");
    plugin_system
        .uninstall("sse-never")
        .expect("uninstall stopped SSE plugin");
}

#[test]
fn compiled_sse_disconnect_releases_connection_threads() {
    let fixture = SseSurfaceFixture::build("sse-never");
    let (plugin_system, app) = started_sse_app(&fixture);
    let server = TestBridge::new(app);
    let disconnected = open_stream(server.base_url(), "/api/fixture/events");
    drop(disconnected);
    std::thread::sleep(Duration::from_millis(50));
    let drop_started = Instant::now();
    drop(server);
    assert!(drop_started.elapsed() < Duration::from_secs(1));
    plugin_system.stop("sse-never").expect("stop SSE fixture");
}

fn compiled_surface_exports(valid_output_schema: bool) -> Vec<ExportDescriptor> {
    let output_schema = if valid_output_schema {
        json!({"type": "object"})
    } else {
        json!({
            "type": "object",
            "required": ["never_returned"],
            "properties": {"never_returned": {"const": true}}
        })
    };
    vec![
        ExportDescriptor {
            id: HTTP_EXPORT_ID.into(),
            name: "Compiled fixture HTTP manifest".into(),
            description: String::new(),
            surface: ExportSurface::HttpRoute {
                method: HttpMethod::Post,
                path_template: "/api/compiled/items/{item_id}".into(),
                stream_mode: HttpStreamMode::Buffered,
                sse_policy: None,
            },
            input_schema: json!({
                "type": "object",
                "required": ["method", "path", "path_parameters", "query", "body", "body_size_bytes"],
                "properties": {
                    "method": {"const": "POST"},
                    "path": {"type": "string"},
                    "path_parameters": {
                        "type": "object",
                        "properties": {"item_id": {"const": "item-7"}},
                        "required": ["item_id"]
                    },
                    "query": {"type": "object"},
                    "body": {"type": "object"},
                    "body_size_bytes": {"type": "integer"}
                }
            }),
            output_schema,
            admission: None,
            execution: ExecutionMode::Foreground,
        },
        ExportDescriptor {
            id: "fixture_panel".into(),
            name: "Compiled fixture".into(),
            description: String::new(),
            surface: ExportSurface::View {
                view_id: VIEW_ID.into(),
                surface: ViewSurface::DashboardPanel,
                asset_path: VIEW_ASSET.into(),
                content_security_policy: "default-src 'none'; script-src 'self'".into(),
                allowed_host_apis: vec!["fixture.read".into()],
                menu_placement: Some(lumvise_plugin_package::ViewMenuPlacement::DesktopSettings),
            },
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        },
    ]
}

fn fixture_manifest(plugin_id: &str, executable_sha256: &str) -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "surface-fixture.release.1".into(),
        },
        plugin_id: plugin_id.into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange {
            min: u32::from(CURRENT_PROTOCOL_VERSION.major),
            max: u32::from(CURRENT_PROTOCOL_VERSION.major),
        },
        targets: BTreeMap::from([(TARGET.into(), EXECUTABLE_PATH.into())]),
        files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable_sha256.into())]),
        exports: Vec::new(),
        host_capabilities: Vec::new(),
    }
}

fn test_view_assets() -> [(&'static str, &'static [u8]); 3] {
    [
        (VIEW_ASSET, VIEW_HTML_BYTES),
        (VIEW_STYLE, VIEW_STYLE_BYTES),
        (VIEW_SCRIPT, VIEW_SCRIPT_BYTES),
    ]
}

fn view_asset(path: &str) -> &'static [u8] {
    test_view_assets()
        .into_iter()
        .find_map(|(candidate, bytes)| (candidate == path).then_some(bytes))
        .unwrap_or_else(|| panic!("missing compiled View fixture asset `{path}`"))
}
struct TestBridge {
    _runtime_root: TempDir,
    _bridge: AppCoreDesktopBridge,
    connection: RuntimeConnection,
}

struct BridgeEndpoint<'a> {
    base_url: &'a str,
    credential: &'a str,
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

    fn base_url(&self) -> BridgeEndpoint<'_> {
        BridgeEndpoint {
            base_url: &self.connection.app_bridge_base_url,
            credential: &self.connection.app_bridge_credential,
        }
    }
}

fn started_sse_app(fixture: &SseSurfaceFixture) -> (Arc<PluginSystem>, Arc<AppCore>) {
    let plugin_system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(AllowViewBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    plugin_system
        .install(&fixture.installed)
        .expect("install SSE fixture");
    plugin_system
        .start(fixture.installed.plugin_id())
        .expect("start SSE fixture");
    let app = Arc::new(
        AppCore::in_memory_with_plugin_system(Arc::clone(&plugin_system))
            .expect("in-memory database"),
    );
    (plugin_system, app)
}

fn open_stream(endpoint: BridgeEndpoint<'_>, path: &str) -> TcpStream {
    let address = endpoint
        .base_url
        .strip_prefix("http://")
        .expect("local HTTP URL");
    let mut stream = TcpStream::connect(address).expect("connect SSE server");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        endpoint.credential
    )
    .expect("write SSE request");
    stream
}

fn read_until_headers(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("SSE read timeout");
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("SSE header byte");
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).expect("UTF-8 SSE headers")
}

fn sse_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "events".into(),
        name: "Fixture events".into(),
        surface: ExportSurface::HttpRoute {
            method: HttpMethod::Get,
            path_template: "/api/fixture/events".into(),
            stream_mode: HttpStreamMode::ServerSentEvents,
            sse_policy: Some(SseStreamPolicy {
                max_events_per_poll: 3,
                poll_interval_ms: 10,
                heartbeat_interval_ms: 30,
                max_backoff_ms: 100,
            }),
        },
        input_schema: json!({
            "type": "object",
            "required": ["method", "path", "path_parameters", "query", "body", "body_size_bytes", "cursor", "max_events"],
            "properties": {
                "method": {"const": "GET"},
                "path": {"const": "/api/fixture/events"},
                "path_parameters": {"type": "object"},
                "query": {"type": "object"},
                "body": {},
                "body_size_bytes": {"type": "integer"},
                "cursor": {"type": ["string", "null"]},
                "max_events": {"type": "integer", "minimum": 1, "maximum": 3}
            },
            "additionalProperties": false
        }),
        output_schema: json!({
            "type": "object",
            "required": ["events", "next_cursor", "done"],
            "properties": {
                "events": {"type": "array", "items": {
                    "type": "object",
                    "required": ["id", "event", "data"],
                    "properties": {
                        "id": {"type": "string"},
                        "event": {"type": "string"},
                        "data": {},
                        "retry_ms": {"type": "integer", "minimum": 0}
                    },
                    "additionalProperties": false
                }},
                "next_cursor": {"type": ["string", "null"]},
                "done": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

struct TestHttpResponse {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}

impl TestHttpResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("JSON response")
    }
}

fn send_request(
    endpoint: BridgeEndpoint<'_>,
    method: &str,
    path: &str,
    body: &[u8],
) -> TestHttpResponse {
    let address = endpoint
        .base_url
        .strip_prefix("http://")
        .expect("local HTTP URL");
    let mut stream = TcpStream::connect(address).expect("connect HTTP server");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        endpoint.credential,
        body.len()
    )
    .expect("write headers");
    stream.write_all(body).expect("write body");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    parse_response(&response)
}

fn parse_response(response: &[u8]) -> TestHttpResponse {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response headers");
    let headers = std::str::from_utf8(&response[..split]).expect("UTF-8 headers");
    let mut lines = headers.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .expect("HTTP status");
    let response_headers = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    let content_type = response_headers
        .get("Content-Type")
        .cloned()
        .expect("content type");
    TestHttpResponse {
        status,
        content_type,
        body: response[split + 4..].to_vec(),
        headers: response_headers,
    }
}

fn runtime_fixture_binary_bytes() -> Vec<u8> {
    RUNTIME_FIXTURE_BINARY
        .get_or_init(|| {
            let workspace = tempfile::tempdir().expect("shared runtime fixture build workspace");
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
            std::fs::read(target_dir.join("debug/plugin-runtime-fixture"))
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

fn replace_view_asset(path: PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("make View asset writable for tamper proof");
    std::fs::write(path, b"tampered unsigned bytes").expect("tamper View asset");
}

fn replace_view_asset_with_symlink(root: &Path, relative: &str, workspace: &Path) {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let path = root.join(relative);
    let parent = path.parent().expect("View asset parent");
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755))
        .expect("make View directory writable");
    std::fs::remove_file(&path).expect("remove signed asset");
    let outside = workspace.join("outside.css");
    std::fs::write(&outside, b"body { color: red; }").expect("outside asset");
    symlink(outside, path).expect("replace asset with symlink");
}
