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
    CompiledPluginUninstallPolicy, PluginInvocationRequest, PluginInvocationStatus,
    PluginProductionConfig,
};
use lumvise_db_core::{LocalPersistence, RelationalPersistence, SemanticPersistence};
use lumvise_frontend_core::FrontendCore;
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_plugin_package::{
    BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility,
    PluginManifest, ProtocolRange, PublisherIdentity, build_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use lumvise_plugin_runtime::{PluginRepositoryConfig, PluginRuntimeConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TARGET: &str = "app-core-production-test";
const EXECUTABLE_PATH: &str = "bin/plugin-runtime-fixture";
const GOOD_PLUGIN_ID: &str = "plugin.bootstrap-ready";
const BROKEN_PLUGIN_ID: &str = "handshake-timeout";
const PLUGIN_VERSION: &str = "1.0.0";
static RUNTIME_FIXTURE_BINARY: OnceLock<Vec<u8>> = OnceLock::new();

struct SignedFixturePackage {
    _workspace: TempDir,
    archive: PathBuf,
}

impl SignedFixturePackage {
    fn build(plugin_id: &str, signing_key: &SigningKey, executable: &[u8]) -> Self {
        Self::build_version(plugin_id, PLUGIN_VERSION, signing_key, executable)
    }

    fn build_version(
        plugin_id: &str,
        version: &str,
        signing_key: &SigningKey,
        executable: &[u8],
    ) -> Self {
        let workspace = tempfile::tempdir().expect("fixture workspace");
        let mut manifest = fixture_manifest(plugin_id, &hex::encode(Sha256::digest(executable)));
        manifest.plugin_version = version.into();
        manifest.exports[0].name = format!("Fixture command {version}");
        let archive = workspace.path().join(format!("{plugin_id}.lvp"));
        build_package(
            &archive,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable.to_vec())]),
                signing_key,
            },
        )
        .expect("fixture package");
        Self {
            _workspace: workspace,
            archive,
        }
    }
}

fn production_test_app(config: PluginProductionConfig) -> Arc<AppCore> {
    let persistence = Arc::new(LocalPersistence::in_memory().expect("in-memory persistence"));
    let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn RelationalPersistence> = persistence;
    Arc::new(
        AppCore::new_production(
            semantic,
            relational,
            FrontendCore::default(),
            LlmProviderRegistry::empty(),
            config,
        )
        .expect("production app"),
    )
}

fn assert_ready_plugin_version(app: &AppCore, plugin_id: &str, version: &str) -> u32 {
    let published = app
        .plugin_system()
        .published_plugins()
        .expect("ready catalog");
    let entry = published
        .iter()
        .find(|entry| entry.plugin_id == plugin_id)
        .expect("ready plugin catalog entry");
    assert_eq!(entry.exports[0].name, format!("Fixture command {version}"));
    let invocation = app
        .plugin_endpoints()
        .invoke_plugin_mcp_tool(PluginInvocationRequest {
            tool_name: format!("app_plugin.{plugin_id}.fixture.command"),
            arguments: json!({"message": "still runnable"}),
        })
        .expect("invoke selected plugin process");
    assert_eq!(invocation.status, PluginInvocationStatus::Completed);
    assert_eq!(invocation.output["capability_id"], "fixture.command");
    app.plugin_system()
        .active_processes()
        .expect("active processes")
        .into_iter()
        .find(|process| process.plugin_id == plugin_id)
        .expect("ready supervised process")
        .process_id
}

#[test]
fn manual_update_survives_reopen_and_two_stale_bundles() {
    const PLUGIN_ID: &str = "plugin.manual-restart";
    let workspace = tempfile::tempdir().expect("production workspace");
    let repository_root = workspace.path().join("repository");
    let trust_file = workspace.path().join("publisher-trust.json");
    let grants_file = workspace.path().join("host-capability-grants.json");
    let key = SigningKey::from_bytes(&[12; 32]);
    let executable = runtime_fixture_binary();
    let old = SignedFixturePackage::build_version(PLUGIN_ID, "1.0.0", &key, &executable);
    let newer = SignedFixturePackage::build_version(PLUGIN_ID, "2.0.0", &key, &executable);
    write_publisher_trust(&trust_file, &key);
    write_host_capability_grants(&grants_file);
    let host = HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET);
    let config =
        PluginProductionConfig::new(&repository_root, &trust_file, &grants_file, host.clone());
    let app = production_test_app(config.clone());
    app.install_compiled_plugin(&old.archive, true)
        .expect("activate initial version");
    app.install_compiled_plugin(&newer.archive, true)
        .expect("manual newer update");
    let first_process = assert_ready_plugin_version(&app, PLUGIN_ID, "2.0.0");
    assert!(app.enable_compiled_plugin(PLUGIN_ID, "1.0.0").is_err());
    let failed =
        SignedFixturePackage::build_version(PLUGIN_ID, "3.0.0", &key, b"#!/bin/sh\nexit 1\n");
    assert!(app.install_compiled_plugin(&failed.archive, true).is_err());
    assert_ready_plugin_version(&app, PLUGIN_ID, "2.0.0");
    assert_eq!(
        app.compiled_plugin_registry()
            .unwrap()
            .highest_enabled_versions[PLUGIN_ID],
        "2.0.0"
    );
    drop(app);

    let mut repository = PluginRepositoryConfig::new(&repository_root, &trust_file, host)
        .open()
        .expect("reopen repository");
    repository
        .install_bundled_archive(&old.archive)
        .expect("stale bundle first offer");
    repository
        .install_bundled_archive(&old.archive)
        .expect("stale bundle second offer");
    drop(repository);
    let reopened = production_test_app(config);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !reopened.plugin_system().is_active(PLUGIN_ID).unwrap() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let second_process = assert_ready_plugin_version(&reopened, PLUGIN_ID, "2.0.0");
    assert_ne!(
        first_process, second_process,
        "restart must launch a new process"
    );
    assert!(reopened.enable_compiled_plugin(PLUGIN_ID, "1.0.0").is_err());
}

#[test]
fn production_bootstrap_keeps_authenticated_http_server_and_retries_broken_plugin() {
    let signing_key = SigningKey::from_bytes(&[11; 32]);
    let executable = runtime_fixture_binary();
    let good = SignedFixturePackage::build(GOOD_PLUGIN_ID, &signing_key, &executable);
    let broken = SignedFixturePackage::build(BROKEN_PLUGIN_ID, &signing_key, &executable);

    let workspace = tempfile::tempdir().expect("production workspace");
    let repository_root = workspace.path().join("repository");
    let trust_file = workspace.path().join("publisher-trust.json");
    let grants_file = workspace.path().join("host-capability-grants.json");
    let host = HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET);

    write_publisher_trust(&trust_file, &signing_key);
    write_host_capability_grants(&grants_file);

    {
        let mut repository =
            PluginRepositoryConfig::new(&repository_root, &trust_file, host.clone())
                .open()
                .expect("open production repository");
        repository
            .install_archive(&good.archive, true)
            .expect("register ready plugin");
        repository
            .install_archive(&broken.archive, true)
            .expect("register broken plugin");
    }

    let mut runtime_config = PluginRuntimeConfig::default();
    runtime_config.handshake_timeout = Duration::from_millis(2_000);
    runtime_config.shutdown_grace = Duration::from_millis(50);
    let production = PluginProductionConfig::new(&repository_root, trust_file, grants_file, host)
        .with_runtime_config(runtime_config);
    let app = production_test_app(production);

    let coordinator = AppRuntimeCoordinator::new(workspace.path().join("runtime"), |_| Ok(()));
    let owner = match coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("runtime ownership")
    {
        AcquireResult::Owner(owner) => owner,
        AcquireResult::Forwarded(_) => panic!("test runtime unexpectedly forwarded"),
    };
    let bridge =
        AppCoreDesktopBridge::new(Arc::clone(&app), owner).expect("authenticated app bridge");
    let connection = bridge
        .runtime_connection()
        .cloned()
        .expect("ready runtime connection");
    let base_url = connection.app_bridge_base_url.clone();
    let credential = connection.app_bridge_credential.clone();
    let early_query_start = Instant::now();
    let early = plugin_surface_response(&base_url, &credential);
    assert!(
        early_query_start.elapsed() < Duration::from_millis(250),
        "authenticated endpoint should remain responsive after bridge readiness",
    );
    assert!(early.is_object(), "surface response must be JSON");
    assert_ne!(
        plugin_availability(&early, BROKEN_PLUGIN_ID),
        Some("ready"),
        "broken plugin should remain unavailable during bootstrap",
    );

    let partial_ready = wait_for_partial_plugin_readiness(
        &app,
        &base_url,
        &credential,
        Duration::from_millis(1_500),
    );
    assert!(
        partial_ready < Duration::from_millis(1_500),
        "good plugin should publish readiness independently before broken bootstrap path times out",
    );

    wait_for_bootstrap_resolution(&app, Duration::from_secs(3));
    let plugin_registry = app.compiled_plugin_registry().expect("compiled registry");
    let broken_record = plugin_registry
        .records
        .iter()
        .find(|record| record.plugin_id == BROKEN_PLUGIN_ID && record.version == PLUGIN_VERSION)
        .expect("broken plugin registry entry");

    assert!(
        app.plugin_system()
            .is_active(GOOD_PLUGIN_ID)
            .expect("ready plugin")
    );
    assert!(
        !app.plugin_system()
            .is_active(BROKEN_PLUGIN_ID)
            .expect("broken plugin")
    );
    assert!(
        broken_record.enabled,
        "transient broken plugin stays enabled for supervisor retry"
    );
    assert!(
        app.plugin_system()
            .is_installed(BROKEN_PLUGIN_ID)
            .expect("broken plugin stays cataloged for fail-closed identity")
    );
    assert!(
        app.plugin_system()
            .is_installed(GOOD_PLUGIN_ID)
            .expect("good plugin installed")
    );

    // Let the background supervisor run at least one retry cadence and confirm
    // it does not durably disable a transiently broken plugin. The supervisor
    // keeps re-attempting start while the durable desired state stays enabled.
    let supervisor_deadline = Instant::now() + Duration::from_millis(2_500);
    while Instant::now() < supervisor_deadline {
        assert!(
            !app.plugin_system()
                .is_active(BROKEN_PLUGIN_ID)
                .expect("broken plugin never becomes ready"),
        );
        assert!(
            app.compiled_plugin_registry()
                .expect("compiled registry")
                .records
                .iter()
                .find(|record| {
                    record.plugin_id == BROKEN_PLUGIN_ID && record.version == PLUGIN_VERSION
                })
                .expect("broken plugin record")
                .enabled,
            "supervisor must keep retrying instead of disabling a transient failure"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let final_surface = plugin_surface_response(&base_url, &credential);
    assert_eq!(
        plugin_availability(&final_surface, GOOD_PLUGIN_ID),
        Some("ready"),
        "ready plugin surfaces must publish as ready",
    );
    assert_ne!(
        plugin_availability(&final_surface, BROKEN_PLUGIN_ID),
        Some("ready"),
        "broken plugin should remain unavailable",
    );
    assert_plugin_settings_lifecycle(&app);
    assert_failed_plugin_activation_keeps_settings_inactive(&app);
    assert_plugin_settings_updates_latest(&app, &signing_key, &executable);
    for plugin_id in ["builtin.semantic", "builtin.knowledge"] {
        assert_required_plugin_lifecycle(&app, plugin_id, &signing_key, &executable);
    }
}

fn assert_required_plugin_lifecycle(
    app: &AppCore,
    plugin_id: &str,
    key: &SigningKey,
    executable: &[u8],
) {
    let package = SignedFixturePackage::build(plugin_id, key, executable);
    app.install_compiled_plugin(&package.archive, true).unwrap();
    assert_required_plugin_cannot_be_disabled(app, plugin_id, PLUGIN_VERSION);
    let update = SignedFixturePackage::build_version(plugin_id, "1.1.0", key, executable);
    app.install_compiled_plugin(&update.archive, false).unwrap();
    let (status, snapshot) = plugin_settings_request(app, plugin_id, Some(("1.1.0", true)));
    assert_eq!(status, 200, "required plugin update: {snapshot}");
    assert_required_plugin_status(&snapshot, plugin_id);
    assert_required_plugin_cannot_be_disabled(app, plugin_id, "1.1.0");
    app.uninstall_compiled_plugin(
        plugin_id,
        PLUGIN_VERSION,
        CompiledPluginUninstallPolicy::Purge,
    )
    .unwrap();
    assert!(app.plugin_system().is_active(plugin_id).unwrap());
}

fn assert_required_plugin_status(snapshot: &Value, plugin_id: &str) {
    let row = snapshot["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["pluginId"] == plugin_id)
        .unwrap();
    assert_eq!(row["required"], true);
    assert_eq!(row["version"], "1.1.0");
    assert_eq!(row["active"], true);
}

fn assert_required_plugin_cannot_be_disabled(app: &AppCore, plugin_id: &str, version: &str) {
    let registry = app.compiled_plugin_registry().unwrap();
    let (status, failure) = plugin_settings_request(app, plugin_id, Some((version, false)));
    assert_eq!(status, 409, "required plugin deactivation: {failure}");
    assert!(failure["error"].as_str().unwrap().contains(plugin_id));
    assert!(app.disable_compiled_plugin(plugin_id).is_err());
    assert!(
        app.uninstall_compiled_plugin(plugin_id, version, CompiledPluginUninstallPolicy::Purge)
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(app.compiled_plugin_registry().unwrap()).unwrap(),
        serde_json::to_value(registry).unwrap()
    );
    assert!(app.plugin_system().is_active(plugin_id).unwrap());
}

fn plugin_settings_request(
    app: &AppCore,
    plugin_id: &str,
    activation: Option<(&str, bool)>,
) -> (u16, Value) {
    let body = activation
        .map(|(version, enabled)| {
            serde_json::to_vec(&json!({
                "pluginId": plugin_id, "version": version, "enabled": enabled,
            }))
            .unwrap()
        })
        .unwrap_or_default();
    let (status, body) = app.route_app_bridge_request(
        if activation.is_some() { "POST" } else { "GET" },
        "/api/settings/plugins",
        BTreeMap::new(),
        body,
    );
    (
        status,
        serde_json::from_str(&body).expect("plugin settings response"),
    )
}

fn assert_plugin_settings_lifecycle(app: &AppCore) {
    for enabled in [false, true] {
        let (status, snapshot) =
            plugin_settings_request(app, GOOD_PLUGIN_ID, Some((PLUGIN_VERSION, enabled)));
        assert_eq!(status, 200, "plugin transition: {snapshot}");
        let rows = snapshot["plugins"].as_array().unwrap();
        let row = rows
            .iter()
            .find(|row| row["pluginId"] == GOOD_PLUGIN_ID)
            .unwrap();
        assert_eq!(row["enabled"], enabled);
        assert_eq!(row["active"], enabled);
        assert_eq!(row["required"], false);
        assert_eq!(
            app.plugin_system().is_active(GOOD_PLUGIN_ID).unwrap(),
            enabled
        );
        assert_eq!(
            plugin_settings_request(app, GOOD_PLUGIN_ID, None).1,
            snapshot
        );
        assert_eq!(
            app.compiled_plugin_registry()
                .unwrap()
                .records
                .iter()
                .find(|record| record.plugin_id == GOOD_PLUGIN_ID)
                .unwrap()
                .enabled,
            enabled
        );
    }
    let (status, _) = app.route_app_bridge_request(
        "POST",
        "/api/settings/plugins",
        BTreeMap::new(),
        serde_json::to_vec(&json!({"pluginId": "missing", "version": "1.0.0", "enabled": true}))
            .unwrap(),
    );
    assert_eq!(status, 409);
    assert!(app.plugin_system().is_active(GOOD_PLUGIN_ID).unwrap());
}

fn assert_failed_plugin_activation_keeps_settings_inactive(app: &AppCore) {
    assert_eq!(
        plugin_settings_request(app, BROKEN_PLUGIN_ID, Some((PLUGIN_VERSION, false))).0,
        200
    );
    let (status, failure) =
        plugin_settings_request(app, BROKEN_PLUGIN_ID, Some((PLUGIN_VERSION, true)));
    assert_eq!(status, 409, "failed activation: {failure}");
    let (_, snapshot) = plugin_settings_request(app, BROKEN_PLUGIN_ID, None);
    let broken = snapshot["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["pluginId"] == BROKEN_PLUGIN_ID)
        .unwrap();
    assert_eq!(broken["enabled"], false);
    assert_eq!(broken["active"], false);
    assert!(app.plugin_system().is_active(GOOD_PLUGIN_ID).unwrap());
}

fn assert_plugin_settings_updates_latest(app: &AppCore, key: &SigningKey, executable: &[u8]) {
    for version in ["1.9.0", "1.10.0"] {
        let package = SignedFixturePackage::build_version(GOOD_PLUGIN_ID, version, key, executable);
        app.install_compiled_plugin(&package.archive, false)
            .unwrap();
    }
    let (_, snapshot) = plugin_settings_request(app, GOOD_PLUGIN_ID, None);
    assert_plugin_settings_row(&snapshot, PLUGIN_VERSION, Some("1.10.0"), true);
    let (status, _) = plugin_settings_request(app, GOOD_PLUGIN_ID, Some(("1.9.0", true)));
    assert_eq!(
        status, 409,
        "Settings must not activate a historical version"
    );
    let (status, updated) = plugin_settings_request(app, GOOD_PLUGIN_ID, Some(("1.10.0", true)));
    assert_eq!(status, 200, "latest release activation: {updated}");
    assert_plugin_settings_row(&updated, "1.10.0", None, true);
    let (_, disabled) = plugin_settings_request(app, GOOD_PLUGIN_ID, Some(("1.10.0", false)));
    assert_plugin_settings_row(&disabled, "1.10.0", None, false);
    assert_failed_plugin_update_restores_selected_version(app, key);
}

fn assert_failed_plugin_update_restores_selected_version(app: &AppCore, key: &SigningKey) {
    assert_eq!(
        plugin_settings_request(app, GOOD_PLUGIN_ID, Some(("1.10.0", true))).0,
        200
    );
    let failed =
        SignedFixturePackage::build_version(GOOD_PLUGIN_ID, "1.11.0", key, b"#!/bin/sh\nexit 1\n");
    app.install_compiled_plugin(&failed.archive, false).unwrap();
    let (status, failure) = plugin_settings_request(app, GOOD_PLUGIN_ID, Some(("1.11.0", true)));
    assert_eq!(status, 409, "failed update: {failure}");
    let (_, snapshot) = plugin_settings_request(app, GOOD_PLUGIN_ID, None);
    assert_plugin_settings_row(&snapshot, "1.10.0", Some("1.11.0"), true);
    assert!(app.plugin_system().is_active(GOOD_PLUGIN_ID).unwrap());
    let registry = app.compiled_plugin_registry().unwrap();
    assert_eq!(registry.highest_enabled_versions[GOOD_PLUGIN_ID], "1.10.0");
    assert!(app.enable_compiled_plugin(GOOD_PLUGIN_ID, "1.10.0").is_ok());
}

fn assert_plugin_settings_row(snapshot: &Value, version: &str, update: Option<&str>, active: bool) {
    let rows: Vec<_> = snapshot["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["pluginId"] == GOOD_PLUGIN_ID)
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "exactly one Settings row per plugin: {rows:?}"
    );
    assert_eq!(rows[0]["version"], version);
    assert_eq!(rows[0]["updateVersion"], json!(update));
    assert_eq!(rows[0]["enabled"], active);
    assert_eq!(rows[0]["active"], active);
}

fn wait_for_partial_plugin_readiness(
    app: &AppCore,
    base_url: &str,
    credential: &str,
    timeout: Duration,
) -> Duration {
    let deadline = Instant::now() + timeout;
    loop {
        let good_active = app
            .plugin_system()
            .is_active(GOOD_PLUGIN_ID)
            .expect("good plugin active check");
        let broken_active = app
            .plugin_system()
            .is_active(BROKEN_PLUGIN_ID)
            .expect("broken plugin active check");
        if good_active && !broken_active {
            let partial_surface = plugin_surface_response(base_url, credential);
            assert_eq!(
                plugin_availability(&partial_surface, GOOD_PLUGIN_ID),
                Some("ready"),
                "good plugin should publish MCP surfaces when ready",
            );
            assert_ne!(
                plugin_availability(&partial_surface, BROKEN_PLUGIN_ID),
                Some("ready"),
                "broken plugin should not be ready while degraded",
            );
            return timeout - (deadline - Instant::now());
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for partial bootstrap readiness");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn write_publisher_trust(path: &Path, signing_key: &SigningKey) {
    let body = serde_json::json!({
        "schema_version": 1,
        "keys": [{
            "publisher_id": "lumvise.test",
            "key_id": "bootstrap.release.1",
            "public_key_hex": hex::encode(signing_key.verifying_key().to_bytes()),
        }],
    });
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&body).expect("trust payload"),
    )
    .expect("write trust store");
}

fn write_host_capability_grants(path: &Path) {
    let body = serde_json::json!({
        "schema_version": 1,
        "grants": [],
    });
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&body).expect("grants payload"),
    )
    .expect("write host grants");
}

fn fixture_manifest(plugin_id: &str, executable_sha256: &str) -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "bootstrap.release.1".into(),
        },
        plugin_id: plugin_id.to_owned(),
        plugin_version: PLUGIN_VERSION.to_owned(),
        protocol: ProtocolRange {
            min: u32::from(CURRENT_PROTOCOL_VERSION.major),
            max: u32::from(CURRENT_PROTOCOL_VERSION.major),
        },
        targets: BTreeMap::from([(TARGET.to_owned(), EXECUTABLE_PATH.to_owned())]),
        files: BTreeMap::from([(EXECUTABLE_PATH.to_owned(), executable_sha256.to_owned())]),
        exports: vec![fixture_export()],
        host_capabilities: Vec::new(),
    }
}

fn fixture_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "fixture.command".into(),
        name: "Fixture command".into(),
        surface: ExportSurface::McpTool,
        input_schema: json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "additionalProperties": false,
        }),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

fn runtime_fixture_binary() -> Vec<u8> {
    RUNTIME_FIXTURE_BINARY
        .get_or_init(|| {
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
            std::fs::read(target_dir.join("debug/plugin-runtime-fixture"))
                .expect("runtime fixture binary")
        })
        .clone()
}

fn plugin_surface_response(base_url: &str, credential: &str) -> Value {
    let authority = base_url.strip_prefix("http://").expect("HTTP base URL");
    let mut stream = TcpStream::connect(authority).expect("connect scoped MCP server");
    write!(
        stream,
        "GET /api/mcp/plugins/surface HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {credential}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let (head, body) = response.split_once("\r\n\r\n").expect("HTTP response body");
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "unexpected response: {head}"
    );
    serde_json::from_str(body).expect("surface json")
}

fn plugin_availability<'a>(surface: &'a Value, plugin_id: &str) -> Option<&'a str> {
    surface
        .get("capabilities")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("plugin_id").and_then(Value::as_str) == Some(plugin_id))?
        .get("availability")?
        .as_str()
}

fn wait_for_bootstrap_resolution(app: &AppCore, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let good_active = app
            .plugin_system()
            .is_active(GOOD_PLUGIN_ID)
            .expect("good plugin active check");
        let broken_active = app
            .plugin_system()
            .is_active(BROKEN_PLUGIN_ID)
            .expect("broken plugin active check");
        let broken_enabled = app
            .compiled_plugin_registry()
            .expect("compiled registry")
            .records
            .iter()
            .find(|record| record.plugin_id == BROKEN_PLUGIN_ID && record.version == PLUGIN_VERSION)
            .expect("broken plugin record")
            .enabled;
        // The broken plugin reaches its retryable desired state: inactive
        // because its handshake keeps timing out, but still enabled because a
        // transient start failure must not durably clear desired state.
        if good_active && !broken_active && broken_enabled {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("production bootstrap did not reach transient retryable state");
}
