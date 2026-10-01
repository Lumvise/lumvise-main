use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Barrier, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use ed25519_dalek::{Signer, SigningKey};
use lumvise_plugin_package::{
    BackgroundDeliveryPolicy, ExclusiveLaneOperation, ExclusiveLanePolicy, ExecutionMode,
    ExportDescriptor, ExportSurface, HostCapabilityRequirement, HostCompatibility, InstalledPlugin,
    InvocationAdmissionPolicy, PluginManifest, ProtocolRange, PublisherIdentity,
    PublisherTrustStore, install_verified_package, verify_package,
};
use lumvise_plugin_protocol::{CURRENT_PROTOCOL_VERSION, WireOutcome};
use lumvise_plugin_runtime::{
    DenyAllHostCapabilityBroker, ExclusiveInvocationLanes, ExclusiveLaneSnapshot,
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PLUGIN_INVOCATION_DEADLINE,
    PluginRepository, PluginRepositoryError, PluginRuntimeConfig, PluginRuntimeError,
    PluginSandbox, PluginSandboxError, PluginSandboxRequest, PluginSystem, SchemaDirection,
    UninstallPolicy,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

const TARGET: &str = "runtime-test-host";
const EXECUTABLE_PATH: &str = "bin/plugin-runtime-fixture";
static NEXT_TEST_INVOCATION_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) trait ControlledTestInvoke {
    fn invoke(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: serde_json::Value,
    ) -> Result<WireOutcome, PluginRuntimeError>;
}

impl ControlledTestInvoke for PluginSystem {
    fn invoke(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: serde_json::Value,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let context = lumvise_plugin_runtime::PluginInvocationContext::new(
            format!(
                "runtime-test-{plugin_id}-{export_id}-{}",
                NEXT_TEST_INVOCATION_ID.fetch_add(1, Ordering::Relaxed)
            ),
            "runtime-test-owner",
            lumvise_plugin_runtime::PluginInvocationClass::Foreground,
            Instant::now() + PLUGIN_INVOCATION_DEADLINE,
        );
        self.invoke_controlled(lumvise_plugin_runtime::PluginInvocationRequest::new(
            plugin_id, export_id, input, context,
        ))
        .map_err(|error| error.into_runtime_error())
    }
}

pub(crate) struct InstalledFixture {
    workspace: TempDir,
    archive: PathBuf,
    signing_key: SigningKey,
    pub(crate) package: InstalledPlugin,
}

impl InstalledFixture {
    pub(crate) fn new(plugin_id: &str) -> Self {
        let workspace = tempfile::tempdir().expect("fixture workspace");
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let archive = write_package(workspace.path(), plugin_id, &signing_key);
        let host = HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET);
        let verified = verify_package(&archive, &signing_key.verifying_key(), &host)
            .expect("fixture package verification");
        let package = install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("fixture package installation");
        Self {
            workspace,
            archive,
            signing_key,
            package,
        }
    }
}

impl Drop for InstalledFixture {
    fn drop(&mut self) {
        make_writable(self.workspace.path());
    }
}

fn write_package(root: &Path, plugin_id: &str, signing_key: &SigningKey) -> PathBuf {
    let executable = std::fs::read(env!("CARGO_BIN_EXE_plugin-runtime-fixture"))
        .expect("read fixture executable");
    let manifest = fixture_manifest(plugin_id, &executable);
    let manifest_bytes = serde_json::to_vec(&manifest).expect("canonical fixture manifest");
    let signature = signing_key.sign(&signature_payload(&manifest_bytes));
    let path = root.join(format!("{plugin_id}.lvp"));
    let mut archive = ZipWriter::new(File::create(&path).expect("create fixture package"));
    write_entry(&mut archive, "manifest.json", &manifest_bytes);
    write_entry(&mut archive, "signature.ed25519", &signature.to_bytes());
    write_entry(&mut archive, EXECUTABLE_PATH, &executable);
    archive.finish().expect("finish fixture package");
    path
}

fn fixture_manifest(plugin_id: &str, executable: &[u8]) -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "runtime.release.1".into(),
        },
        plugin_id: plugin_id.to_owned(),
        plugin_version: "1.0.0".to_owned(),
        protocol: ProtocolRange {
            min: u32::from(CURRENT_PROTOCOL_VERSION.major),
            max: u32::from(CURRENT_PROTOCOL_VERSION.major),
        },
        targets: BTreeMap::from([(TARGET.to_owned(), EXECUTABLE_PATH.to_owned())]),
        files: BTreeMap::from([(
            EXECUTABLE_PATH.to_owned(),
            hex::encode(Sha256::digest(executable)),
        )]),
        exports: fixture_exports(plugin_id),
        host_capabilities: if plugin_id.starts_with("host-call-")
            || plugin_id.starts_with("mux-host")
        {
            vec![HostCapabilityRequirement {
                id: "clock.read".to_owned(),
                version: "^1.0".to_owned(),
            }]
        } else if plugin_id.starts_with("plugin-invoke-") && plugin_id != "plugin-invoke-undeclared"
        {
            vec![HostCapabilityRequirement {
                id: "plugin.invoke".to_owned(),
                version: "^1.0".to_owned(),
            }]
        } else {
            Vec::new()
        },
    }
}

fn fixture_exports(plugin_id: &str) -> Vec<ExportDescriptor> {
    let mut exports: Vec<_> = [
        "echo.value",
        "fixture.crash",
        "fixture.timeout",
        "fixture.echo",
        "fixture.host-call",
    ]
    .into_iter()
    .map(|id| ExportDescriptor {
        description: String::new(),
        id: id.to_owned(),
        name: id.to_owned(),
        surface: ExportSurface::Command,
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    })
    .collect();
    match plugin_id {
        "schema-guard" => exports.push(schema_guard_export()),
        "schema-invalid-output" => exports.push(invalid_output_export()),
        "schema-compile-failure" => exports.push(invalid_schema_export()),
        "scoped-catalog" => exports.extend([
            scoped_export("fixture.assistant", "assistant_session"),
            scoped_export("fixture.other", "other_session"),
        ]),
        "background-catalog" => exports.extend(background_exports()),
        id if id.starts_with("lane-") => exports.extend(exclusive_lane_exports()),
        id if id.starts_with("mux-") => exports.extend(mux_exports()),
        _ => {}
    }
    exports.extend([
        target_echo_export(),
        target_invalid_output_export(),
        proxy_invoke_export(),
    ]);
    exports
}

/// Exports exercised by the multiplexed `mux-` fixture mode.
fn mux_exports() -> [ExportDescriptor; 2] {
    ["mux.barrier", "mux.host-call"].map(|id| ExportDescriptor {
        description: String::new(),
        id: id.to_owned(),
        name: id.to_owned(),
        surface: ExportSurface::Command,
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    })
}

fn exclusive_lane_exports() -> [ExportDescriptor; 2] {
    [
        exclusive_lane_export("lane.acquire", ExclusiveLaneOperation::Acquire),
        exclusive_lane_export("lane.release", ExclusiveLaneOperation::ReleaseOnOutput),
    ]
}

fn exclusive_lane_export(id: &str, operation: ExclusiveLaneOperation) -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: id.into(),
        name: id.into(),
        surface: ExportSurface::Command,
        input_schema: lane_input_schema(),
        output_schema: lane_output_schema(),
        admission: Some(InvocationAdmissionPolicy::ExclusiveLane(
            ExclusiveLanePolicy {
                lane_id: "session".into(),
                operation,
                owner_argument: "owner_id".into(),
                session_argument: "session_id".into(),
                queue_argument: "queue".into(),
                replace_argument: "replace".into(),
                timeout_ms_argument: "timeout_ms".into(),
                default_timeout_ms: 500,
                max_timeout_ms: 1_000,
                max_queue_depth: 1,
                response_session_pointer: "/input/session_id".into(),
                terminal_pointer: "/input/phase".into(),
                terminal_values: vec!["done".into()],
            },
        )),
        execution: ExecutionMode::Foreground,
    }
}

fn lane_input_schema() -> serde_json::Value {
    json!({"type": "object", "properties": {
        "owner_id": {"type": "string"}, "session_id": {"type": "string"},
        "queue": {"type": "boolean"}, "replace": {"type": "boolean"},
        "timeout_ms": {"type": "integer"}, "phase": {"type": "string"},
        "force_fail": {"type": "boolean"}
    }})
}

fn lane_output_schema() -> serde_json::Value {
    json!({"type": "object", "properties": {"input": {"type": "object", "properties": {
        "session_id": {"type": "string"}, "phase": {"type": "string"}
    }}}})
}

fn background_exports() -> [ExportDescriptor; 2] {
    [
        ExportDescriptor {
            description: String::new(),
            id: "fixture.recurring".into(),
            name: "Recurring fixture".into(),
            surface: ExportSurface::RecurringTask {
                interval_seconds: 60,
                delivery: fixture_delivery_policy(),
            },
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Background,
        },
        ExportDescriptor {
            description: String::new(),
            id: "fixture.storage".into(),
            name: "Storage fixture".into(),
            surface: ExportSurface::StorageTrigger {
                event_kinds: vec!["semantic.element.upserted".into()],
                entity_kinds: vec!["semantic_element".into()],
                delivery: fixture_delivery_policy(),
            },
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Background,
        },
    ]
}

fn fixture_delivery_policy() -> BackgroundDeliveryPolicy {
    BackgroundDeliveryPolicy {
        max_attempts: 3,
        initial_backoff_ms: 10,
        max_backoff_ms: 100,
        dead_letter_max_entries: 10,
        dead_letter_retention_seconds: 3_600,
    }
}

fn proxy_invoke_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "proxy.invoke".into(),
        name: "Invocation proxy".into(),
        surface: ExportSurface::McpTool,
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

fn target_echo_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "target.echo".into(),
        name: "Target echo".into(),
        surface: ExportSurface::McpTool,
        input_schema: json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
            "additionalProperties": false
        }),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

fn target_invalid_output_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "target.invalid-output".into(),
        name: "Invalid target output".into(),
        surface: ExportSurface::McpTool,
        input_schema: json!({"type": "object"}),
        output_schema: json!({
            "type": "object",
            "properties": {"capability_id": {"type": "integer"}},
            "required": ["capability_id"]
        }),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

#[derive(Default)]
pub(crate) struct AllowPluginInvokeBroker;

impl HostCapabilityBroker for AllowPluginInvokeBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        if request.capability_id == "plugin.invoke" && request.required_version == "^1.0" {
            return Ok(json!({}));
        }
        Err(HostCapabilityError::new(
            request.capability_id,
            "host_capability_denied",
            "test policy denied capability",
            false,
        ))
    }
}

fn scoped_export(id: &str, scope: &str) -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: id.into(),
        name: id.into(),
        surface: ExportSurface::ScopedMcpTool {
            scope: scope.into(),
        },
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

fn schema_guard_export() -> ExportDescriptor {
    let input_schema = json!({
        "type": "object",
        "properties": {
            "profile": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["name", "tags"],
                "additionalProperties": false
            }
        },
        "required": ["profile"],
        "additionalProperties": false
    });
    ExportDescriptor {
        description: String::new(),
        id: "schema.nested".to_owned(),
        name: "Nested schema".to_owned(),
        surface: ExportSurface::Command,
        input_schema: input_schema.clone(),
        output_schema: json!({
            "type": "object",
            "properties": {
                "capability_id": {"const": "schema.nested"},
                "input": input_schema
            },
            "required": ["capability_id", "input"],
            "additionalProperties": false
        }),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

fn invalid_output_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "schema.invalid-output".to_owned(),
        name: "Invalid output fixture".to_owned(),
        surface: ExportSurface::Command,
        input_schema: json!({"type": "object"}),
        output_schema: json!({
            "type": "object",
            "properties": {"capability_id": {"type": "integer"}},
            "required": ["capability_id"]
        }),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

fn invalid_schema_export() -> ExportDescriptor {
    ExportDescriptor {
        description: String::new(),
        id: "schema.compile-failure".to_owned(),
        name: "Invalid schema fixture".to_owned(),
        surface: ExportSurface::Command,
        input_schema: json!({"type": "not-a-json-schema-type"}),
        output_schema: json!({"type": "object"}),
        admission: None,
        execution: ExecutionMode::Foreground,
    }
}

#[derive(Default)]
struct RecordingClockBroker {
    requests: Mutex<Vec<HostCapabilityRequest>>,
}

impl HostCapabilityBroker for RecordingClockBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        self.requests
            .lock()
            .expect("recording broker lock")
            .push(request);
        Ok(json!({"unix_seconds": 42}))
    }
}

pub(crate) struct BlockingClockBroker {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl BlockingClockBroker {
    pub(crate) fn new() -> (Arc<Self>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let broker = Arc::new(Self {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        });
        (broker, entered_rx, release_tx)
    }
}

impl HostCapabilityBroker for BlockingClockBroker {
    fn invoke(
        &self,
        _request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        if let Some(entered) = self.entered.lock().expect("entered lock").take() {
            entered.send(()).expect("signal broker entry");
        }
        self.release
            .lock()
            .expect("release lock")
            .recv()
            .expect("release broker");
        Ok(json!({"unix_seconds": 42}))
    }
}

#[derive(Default)]
struct CatalogReadingBroker {
    system: OnceLock<Weak<PluginSystem>>,
    observed_ready_plugins: Mutex<Vec<String>>,
}

impl HostCapabilityBroker for CatalogReadingBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let system = self
            .system
            .get()
            .and_then(Weak::upgrade)
            .expect("catalog system configured");
        assert!(
            system
                .is_installed(&request.plugin_id)
                .expect("installed read")
        );
        assert!(
            !system
                .exports(&request.plugin_id)
                .expect("exports read")
                .is_empty()
        );
        let ready = system
            .published_plugins()
            .expect("published snapshot")
            .into_iter()
            .map(|plugin| plugin.plugin_id)
            .collect();
        *self
            .observed_ready_plugins
            .lock()
            .expect("observed plugins lock") = ready;
        Ok(json!({"unix_seconds": 42}))
    }
}

fn write_entry(archive: &mut ZipWriter<File>, path: &str, bytes: &[u8]) {
    archive
        .start_file(path, SimpleFileOptions::default())
        .expect("start fixture entry");
    archive.write_all(bytes).expect("write fixture entry");
}

fn signature_payload(manifest: &[u8]) -> Vec<u8> {
    [b"LUMVISE_PLUGIN_PACKAGE_V1\0".as_slice(), manifest].concat()
}

pub(crate) fn fast_config() -> PluginRuntimeConfig {
    let mut config =
        PluginRuntimeConfig::default().with_controlled_test_deadline(Duration::from_millis(150));
    config.handshake_timeout = Duration::from_secs(10);
    config.shutdown_grace = Duration::from_millis(100);
    config
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

fn test_plugin_system() -> PluginSystem {
    test_plugin_system_with_broker(fast_config(), Arc::new(DenyAllHostCapabilityBroker))
}

pub(crate) fn test_plugin_system_with_lanes(lanes: Arc<ExclusiveInvocationLanes>) -> PluginSystem {
    PluginSystem::with_broker_sandbox_and_lanes(
        fast_config(),
        Arc::new(DenyAllHostCapabilityBroker),
        Arc::new(TestSubprocessSandbox),
        lanes,
    )
}

pub(crate) fn test_plugin_system_with_broker(
    config: PluginRuntimeConfig,
    broker: Arc<dyn HostCapabilityBroker>,
) -> PluginSystem {
    PluginSystem::with_broker_and_sandbox(config, broker, Arc::new(TestSubprocessSandbox))
}

fn fixture_trust(fixture: &InstalledFixture) -> Arc<PublisherTrustStore> {
    let mut trust = PublisherTrustStore::new();
    trust
        .add_key(
            "lumvise.test",
            "runtime.release.1",
            fixture.signing_key.verifying_key(),
        )
        .expect("fixture publisher trust");
    Arc::new(trust)
}

fn open_fixture_repository(root: &Path, trust: Arc<PublisherTrustStore>) -> PluginRepository {
    PluginRepository::open(
        root,
        trust,
        HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
    )
    .expect("open fixture repository")
}

#[cfg(unix)]
fn make_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            make_writable(&path);
        }
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755));
}
