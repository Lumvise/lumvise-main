#![cfg(unix)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use ed25519_dalek::SigningKey;
use lumvise_app_core::{AppCore, PluginInvocationStatus, ScopedMcpHttpServer};
use lumvise_plugin_package::{
    BackgroundDeliveryPolicy, BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface,
    HostCompatibility, PluginManifest, ProtocolRange, PublisherIdentity, build_package,
    install_verified_package, verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use lumvise_plugin_runtime::{
    PluginInvocationClass, PluginInvocationContext, PluginInvocationRequest, PluginRuntimeConfig,
    PluginSandbox, PluginSandboxError, PluginSandboxRequest, PluginSystem,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TARGET: &str = "app-core-background-test";
const EXECUTABLE_PATH: &str = "bin/plugin-runtime-fixture";
static RUNTIME_FIXTURE_BINARY: OnceLock<Vec<u8>> = OnceLock::new();

struct TestSubprocessSandbox;

impl PluginSandbox for TestSubprocessSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        Ok(Command::new(request.executable))
    }
}

struct BackgroundFixture {
    _workspace: TempDir,
    installed: lumvise_plugin_package::InstalledPlugin,
}

impl BackgroundFixture {
    fn build(plugin_id: &str, include_storage: bool, max_attempts: u32) -> Self {
        let workspace = tempfile::tempdir().expect("background fixture workspace");
        let executable = runtime_fixture_binary();
        let manifest = background_manifest(
            plugin_id,
            &hex::encode(Sha256::digest(&executable)),
            include_storage,
            max_attempts,
        );
        let signing_key = SigningKey::from_bytes(&[61; 32]);
        let archive = workspace.path().join(format!("{plugin_id}.lvp"));
        build_package(
            &archive,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable)]),
                signing_key: &signing_key,
            },
        )
        .expect("signed background package");
        let verified = verify_package(
            &archive,
            &signing_key.verifying_key(),
            &HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
        )
        .expect("verified background package");
        let installed = install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("installed background package");
        Self {
            _workspace: workspace,
            installed,
        }
    }
}

#[test]
fn recurring_delivery_is_due_once_per_signed_cadence() {
    let fixture = BackgroundFixture::build("background-ok", false, 3);
    let (app, system) = app_with_fixture(&fixture);

    let first = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(10)
        .expect("first cadence");
    let duplicate = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(10)
        .expect("same cadence");
    let later = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(70)
        .expect("later cadence");

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, PluginInvocationStatus::Completed);
    assert!(duplicate.is_empty());
    assert_eq!(later.len(), 1);
    system.stop("background-ok").expect("stop fixture");
}

#[test]
fn scoped_app_runtime_drives_recurring_tasks_without_manual_pumping() {
    let fixture = BackgroundFixture::build("background-runtime", false, 3);
    let (app, system) = app_with_fixture(&fixture);
    let app = Arc::new(app);
    let server = ScopedMcpHttpServer::spawn(Arc::clone(&app)).expect("scoped app runtime");

    std::thread::sleep(Duration::from_secs(2));

    let manually_due = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(chrono::Utc::now().timestamp())
        .expect("manual recurring check after runtime tick");
    assert!(
        manually_due.is_empty(),
        "production runtime must consume the initially due recurring delivery"
    );
    drop(server);
    system.stop("background-runtime").expect("stop fixture");
}

#[test]
fn crashed_plugin_retries_to_signed_limit_without_static_fallback() {
    let fixture = BackgroundFixture::build("crash", false, 2);
    let (app, system) = app_with_fixture(&fixture);

    let first = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(10)
        .expect("first crashing attempt");
    assert_eq!(first[0].attempt, 1);
    assert_eq!(first[0].status, PluginInvocationStatus::Failed);
    assert!(!system.is_active("crash").expect("catalog"));

    system.start("crash").expect("restart for retry");
    let second = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(11)
        .expect("second crashing attempt");
    assert_eq!(second[0].attempt, 2);
    assert_eq!(second[0].status, PluginInvocationStatus::Failed);

    system.start("crash").expect("restart after dead letter");
    let exhausted = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(12)
        .expect("dead letter remains terminal");
    assert!(exhausted.is_empty());
    system.stop("crash").expect("stop restarted fixture");
}

#[test]
fn nonretryable_plugin_failure_dead_letters_immediately() {
    let fixture = BackgroundFixture::build("target-failed", false, 3);
    let (app, system) = app_with_fixture(&fixture);

    let first = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(10)
        .expect("failed delivery");
    let repeated = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(11)
        .expect("dead letter not retried");

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempt, 1);
    assert_eq!(first[0].status, PluginInvocationStatus::Failed);
    assert!(repeated.is_empty());
    system.stop("target-failed").expect("stop fixture");
}

#[test]
fn timed_out_plugin_retries_and_is_unpublished_fail_closed() {
    let fixture = BackgroundFixture::build("timeout", false, 2);
    let (app, system) = app_with_fixture(&fixture);

    let first = app
        .plugin_endpoints()
        .run_due_plugin_recurring_tasks(10)
        .expect("timed out delivery");

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempt, 1);
    assert_eq!(first[0].status, PluginInvocationStatus::Failed);
    assert!(!system.is_active("timeout").expect("catalog"));
}

#[test]
fn hung_background_plugin_does_not_consume_unrelated_foreground_capacity() {
    let timeout = BackgroundFixture::build("timeout", false, 2);
    let ready = BackgroundFixture::build("background-ok", false, 2);
    let (app, system) = app_with_fixtures([&timeout, &ready]);
    let app = Arc::new(app);
    let runner = Arc::clone(&app);
    let background = std::thread::spawn(move || {
        runner
            .plugin_endpoints()
            .run_due_plugin_recurring_tasks(10)
            .expect("background run")
    });
    let wait_until = Instant::now() + Duration::from_secs(1);
    while !system
        .invocation_admission_snapshot("timeout")
        .expect("admission snapshot")
        .executing
    {
        assert!(
            Instant::now() < wait_until,
            "timeout delivery did not start"
        );
        std::thread::yield_now();
    }

    let started = Instant::now();
    let outcome = system
        .invoke_controlled(PluginInvocationRequest::new(
            "background-ok",
            "fixture.recurring",
            json!({"foreground": true}),
            PluginInvocationContext::new(
                "foreground-during-background-timeout",
                "background-test-owner",
                PluginInvocationClass::Foreground,
                Instant::now() + Duration::from_secs(1),
            ),
        ))
        .expect("unrelated foreground invocation");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(matches!(
        outcome,
        lumvise_plugin_protocol::WireOutcome::Succeeded { .. }
    ));
    let outcomes = background.join().expect("background thread");
    assert_eq!(outcomes.len(), 2);
}

fn app_with_fixture(fixture: &BackgroundFixture) -> (AppCore, Arc<PluginSystem>) {
    app_with_fixtures([fixture])
}

fn app_with_fixtures<const N: usize>(
    fixtures: [&BackgroundFixture; N],
) -> (AppCore, Arc<PluginSystem>) {
    let system = Arc::new(PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(lumvise_plugin_runtime::DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
    ));
    for fixture in fixtures {
        system
            .install(&fixture.installed)
            .expect("install background fixture");
        system
            .start(fixture.installed.plugin_id())
            .expect("start background fixture");
    }
    let app = app_with_system(Arc::clone(&system));
    (app, system)
}

fn app_with_system(system: Arc<PluginSystem>) -> AppCore {
    AppCore::in_memory_with_plugin_system(system).expect("database")
}

fn background_manifest(
    plugin_id: &str,
    executable_sha256: &str,
    include_storage: bool,
    max_attempts: u32,
) -> PluginManifest {
    let mut exports = vec![recurring_export(max_attempts)];
    if include_storage {
        exports.push(storage_export(max_attempts));
    }
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "background.release.1".into(),
        },
        plugin_id: plugin_id.into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange {
            min: u32::from(CURRENT_PROTOCOL_VERSION.major),
            max: u32::from(CURRENT_PROTOCOL_VERSION.major),
        },
        targets: BTreeMap::from([(TARGET.into(), EXECUTABLE_PATH.into())]),
        files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable_sha256.into())]),
        exports,
        host_capabilities: Vec::new(),
    }
}

fn recurring_export(max_attempts: u32) -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "fixture.recurring".into(),
        name: "Recurring fixture".into(),
        surface: ExportSurface::RecurringTask {
            interval_seconds: 60,
            delivery: delivery_policy(max_attempts),
        },
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Background,
    }
}

fn storage_export(max_attempts: u32) -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "fixture.storage".into(),
        name: "Storage fixture".into(),
        surface: ExportSurface::StorageTrigger {
            event_kinds: vec!["semantic.element.upserted".into()],
            entity_kinds: vec!["semantic_element".into()],
            delivery: delivery_policy(max_attempts),
        },
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Background,
    }
}

fn delivery_policy(max_attempts: u32) -> BackgroundDeliveryPolicy {
    BackgroundDeliveryPolicy {
        max_attempts,
        initial_backoff_ms: 100,
        max_backoff_ms: 1_000,
        dead_letter_max_entries: 10,
        dead_letter_retention_seconds: 3_600,
    }
}

fn runtime_fixture_binary() -> Vec<u8> {
    RUNTIME_FIXTURE_BINARY
        .get_or_init(|| {
            let workspace = tempfile::tempdir().expect("runtime fixture build workspace");
            let executable = compile_runtime_fixture(workspace.path());
            std::fs::read(executable).expect("runtime fixture binary")
        })
        .clone()
}

fn compile_runtime_fixture(root: &Path) -> PathBuf {
    let target_dir = root.join("target");
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
        .expect("build runtime fixture");
    assert!(status.success());
    target_dir.join("debug/plugin-runtime-fixture")
}

fn fast_runtime_config() -> PluginRuntimeConfig {
    let mut config =
        PluginRuntimeConfig::default().with_controlled_test_deadline(Duration::from_secs(3));
    config.handshake_timeout = Duration::from_secs(3);
    config.shutdown_grace = Duration::from_secs(1);
    config
}
