use super::*;
use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    ExecutionMode, ExportDescriptor, ExportSurface, PluginManifest, PluginReleaseArtifactV2,
    ProtocolRange, PublisherIdentity, ReleaseComposition, build_package_from_directory,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[test]
fn production_runtime_admits_one_foreground_and_background_export_to_a_mux_process() {
    let production = PluginProductionConfig::new(
        "/tmp/plugins",
        "/tmp/publishers.json",
        "/tmp/grants.json",
        HostCompatibility::new(1, "test-target"),
    );

    assert_eq!(
        production.runtime.max_concurrent_invocations_per_plugin,
        PRODUCTION_PLUGIN_PIPELINE_CAPACITY
    );
    assert_eq!(
        production
            .runtime
            .export_concurrency
            .get_policy("builtin.knowledge.create_knowledge"),
        lumvise_plugin_runtime::ExportConcurrencyPolicy::Serial
    );
    assert_eq!(
        PluginRuntimeConfig::default().max_concurrent_invocations_per_plugin,
        1,
        "non-production callers retain the conservative single-invocation default"
    );
}

#[test]
fn bundled_release_install_is_idempotent_and_preserves_existing_policy() {
    let workspace = tempfile::tempdir().expect("bundle workspace");
    let release = workspace.path().join("release");
    let state = workspace.path().join("state");
    fs::create_dir_all(&release).expect("release directory");
    let key = SigningKey::from_bytes(&[73; 32]);
    write_test_release(&release, &key);
    let config = PluginProductionConfig::new(
        state.join("repository"),
        state.join("publisher-trust.json"),
        state.join("host-capability-grants.json"),
        HostCompatibility::new(1, "test-target"),
    );

    config
        .install_bundled_release(&release)
        .expect("first bundled install");
    fs::write(
        state.join("host-capability-grants.json"),
        br#"{"schema_version":1,"grants":[]}"#,
    )
    .expect("administrator policy");
    config
        .install_bundled_release(&release)
        .expect("idempotent bundled install");

    let repository = config.repository.open().expect("installed repository");
    assert_eq!(repository.registry().records.len(), 2);
    assert!(
        repository
            .registry()
            .records
            .iter()
            .all(|record| record.enabled)
    );
    assert_eq!(
        fs::read(state.join("host-capability-grants.json")).expect("preserved policy"),
        br#"{"schema_version":1,"grants":[]}"#
    );
    let disabled_id = repository.registry().records[0].plugin_id.clone();
    let mut repository = repository;
    repository
        .disable(&disabled_id)
        .expect("disable bundled plugin");
    drop(repository);
    config
        .install_bundled_release(&release)
        .expect("next launch");
    let repository = config.repository.open().expect("reopened repository");
    assert!(
        !repository
            .registry()
            .records
            .iter()
            .find(|record| record.plugin_id == disabled_id)
            .unwrap()
            .enabled
    );
}

#[test]
fn stale_bundled_release_never_downgrades_a_newer_enabled_version() {
    let workspace = tempfile::tempdir().expect("bundle workspace");
    let key = SigningKey::from_bytes(&[73; 32]);
    let newer = workspace.path().join("newer");
    let older = workspace.path().join("older");
    fs::create_dir_all(&newer).expect("newer release");
    fs::create_dir_all(&older).expect("older release");
    write_test_release_version(&newer, &key, "2.0.0");
    write_test_release_version(&older, &key, "1.0.0");
    let config = test_production_config(workspace.path().join("state"));

    config
        .install_bundled_release(&newer)
        .expect("newer install");
    config
        .install_bundled_release(&older)
        .expect("stale bundle next to the binary");
    config
        .install_bundled_release(&older)
        .expect("same stale bundle on next launch");

    let repository = config.repository.open().expect("installed repository");
    for plugin_id in ["builtin.knowledge", "builtin.semantic"] {
        let enabled = repository
            .registry()
            .records
            .iter()
            .filter(|record| record.plugin_id == plugin_id && record.enabled)
            .map(|record| record.version.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            enabled,
            ["2.0.0"],
            "{plugin_id} must keep its newer version"
        );
        assert!(
            repository
                .registry()
                .records
                .iter()
                .any(|record| record.plugin_id == plugin_id && record.version == "1.0.0"),
            "{plugin_id}: the older bundle stays installed for rollback"
        );
    }
}

#[test]
fn bundled_release_rejects_every_invalid_boundary_before_dynamic_install() {
    for corruption in [
        ReleaseCorruption::SchemaOne,
        ReleaseCorruption::UnknownSchema,
        ReleaseCorruption::Digest,
        ReleaseCorruption::Signature,
        ReleaseCorruption::Target,
        ReleaseCorruption::Membership,
    ] {
        let workspace = tempfile::tempdir().expect("bundle workspace");
        let release = workspace.path().join("release");
        fs::create_dir_all(&release).expect("release directory");
        let key = SigningKey::from_bytes(&[74; 32]);
        write_test_release(&release, &key);
        corrupt_release(&release, corruption);
        let config = test_production_config(workspace.path().join("state"));
        assert!(
            config.install_bundled_release(&release).is_err(),
            "corruption {corruption:?} must fail closed"
        );
    }

    let workspace = tempfile::tempdir().expect("valid bundle workspace");
    let release = workspace.path().join("release");
    fs::create_dir_all(&release).expect("release directory");
    let key = SigningKey::from_bytes(&[75; 32]);
    write_test_release(&release, &key);
    let config = test_production_config(workspace.path().join("state"));
    config
        .install_bundled_release(&release)
        .expect("valid bundled release");
    let dynamic = write_test_archive(&release, &key, "plugin.dynamic", "1.0.0");
    config
        .repository
        .open()
        .expect("repository")
        .install_archive(&release.join(dynamic.archive_path), true)
        .expect("later signed dynamic install");
}

#[derive(Clone, Copy, Debug)]
enum ReleaseCorruption {
    SchemaOne,
    UnknownSchema,
    Digest,
    Signature,
    Target,
    Membership,
}

fn corrupt_release(release: &Path, corruption: ReleaseCorruption) {
    let index_path = release.join("builtins-release.json");
    let mut index: serde_json::Value =
        serde_json::from_slice(&fs::read(&index_path).expect("release index"))
            .expect("parse release index");
    match corruption {
        ReleaseCorruption::SchemaOne => index["schema_version"] = json!(1),
        ReleaseCorruption::UnknownSchema => index["schema_version"] = json!(99),
        ReleaseCorruption::Digest => {
            index["artifacts"][0]["archive_sha256"] = json!("00".repeat(32));
        }
        ReleaseCorruption::Signature => {
            let archive_name = index["artifacts"][0]["archive_path"]
                .as_str()
                .expect("archive path");
            let archive = release.join(archive_name);
            let mut bytes = fs::read(&archive).expect("archive");
            let last = bytes.last_mut().expect("non-empty archive");
            *last ^= 0xff;
            fs::write(&archive, &bytes).expect("tampered archive");
            index["artifacts"][0]["archive_sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
        }
        ReleaseCorruption::Target => {
            index["artifacts"][0]["targets"] = json!(["wrong-target"]);
        }
        ReleaseCorruption::Membership => {
            index["artifacts"]
                .as_array_mut()
                .expect("artifact list")
                .pop();
        }
    }
    fs::write(
        index_path,
        serde_json::to_vec(&index).expect("serialize corrupt index"),
    )
    .expect("write corrupt index");
}

pub(super) fn test_production_config(state: PathBuf) -> PluginProductionConfig {
    PluginProductionConfig::new(
        state.join("repository"),
        state.join("publisher-trust.json"),
        state.join("host-capability-grants.json"),
        HostCompatibility::new(1, "test-target"),
    )
}

fn write_test_release(release: &Path, key: &SigningKey) {
    write_test_release_version(release, key, "1.0.0");
}

fn write_test_release_version(release: &Path, key: &SigningKey, version: &str) {
    let artifacts = ["builtin.knowledge", "builtin.semantic"]
        .map(|plugin_id| write_test_archive(release, key, plugin_id, version))
        .into_iter()
        .collect();
    let index = PluginReleaseIndexV2 {
        schema_version: lumvise_plugin_package::PLUGIN_RELEASE_INDEX_SCHEMA_VERSION,
        product: "lumvise".into(),
        composition: ReleaseComposition::Minimal,
        artifacts,
    };
    fs::write(
        release.join("builtins-release.json"),
        serde_json::to_vec(&index).expect("release index"),
    )
    .expect("write index");
    write_test_policies(release, key);
}

fn write_test_archive(
    release: &Path,
    key: &SigningKey,
    plugin_id: &str,
    version: &str,
) -> PluginReleaseArtifactV2 {
    let payload = release.join(format!("{plugin_id}-payload"));
    fs::create_dir_all(payload.join("bin")).expect("payload directory");
    fs::write(payload.join("bin/plugin"), b"compiled plugin").expect("payload");
    let archive_name = format!("{plugin_id}-{version}.lvp");
    let archive = release.join(&archive_name);
    build_package_from_directory(&archive, test_manifest(plugin_id, version), &payload, key)
        .expect("package");
    PluginReleaseArtifactV2 {
        plugin_id: plugin_id.into(),
        plugin_version: version.into(),
        publisher_id: "lumvise.test".into(),
        key_id: "release.1".into(),
        archive_path: archive_name,
        archive_sha256: format!("{:x}", Sha256::digest(fs::read(archive).expect("archive"))),
        targets: vec!["test-target".into()],
    }
}

pub(super) fn test_manifest(plugin_id: &str, version: &str) -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "release.1".into(),
        },
        plugin_id: plugin_id.into(),
        plugin_version: version.into(),
        protocol: ProtocolRange { min: 1, max: 1 },
        targets: BTreeMap::from([("test-target".into(), "bin/plugin".into())]),
        files: BTreeMap::from([("bin/plugin".into(), "placeholder".into())]),
        exports: vec![ExportDescriptor {
            id: "run".into(),
            name: "Run".into(),
            description: String::new(),
            surface: ExportSurface::Command,
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        }],
        host_capabilities: Vec::new(),
    }
}

fn write_test_policies(release: &Path, key: &SigningKey) {
    let trust = json!({
        "schema_version": 1,
        "keys": [{
            "publisher_id": "lumvise.test",
            "key_id": "release.1",
            "public_key_hex": hex::encode(key.verifying_key().to_bytes()),
            "revoked": false
        }]
    });
    fs::write(
        release.join("publisher-trust.json"),
        serde_json::to_vec(&trust).expect("trust policy"),
    )
    .expect("write trust policy");
    fs::write(
        release.join("host-capability-grants.json"),
        br#"{"schema_version":1,"grants":[{"plugin_id":"builtin.knowledge","capability_id":"storage.plugin"}]}"#,
    )
    .expect("write grants");
}

#[test]
fn granted_compatible_storage_request_executes() {
    let broker = policy_broker(json!({
        "schema_version": 1,
        "grants": [{
            "plugin_id": "plugin.alpha",
            "capability_id": "storage.plugin"
        }]
    }));

    let output = broker
        .invoke(storage_request("plugin.alpha", "^1.0"))
        .expect("granted compatible request");

    assert_eq!(output["table_name"], "notes");
}

mod invocation_control;

#[test]
fn grant_is_bound_to_plugin_and_catalog_version() {
    let broker = policy_broker(json!({
        "schema_version": 1,
        "grants": [{
            "plugin_id": "plugin.alpha",
            "capability_id": "storage.plugin"
        }]
    }));

    let foreign = broker
        .invoke(storage_request("plugin.bravo", "^1.0"))
        .expect_err("foreign plugin denied");
    let incompatible = broker
        .invoke(storage_request("plugin.alpha", "^2.0"))
        .expect_err("incompatible version denied");

    assert!(foreign.to_string().contains("host_capability_denied"));
    assert!(
        incompatible
            .to_string()
            .contains("host_capability_version_mismatch")
    );
}

#[test]
fn duplicate_grant_is_rejected_during_policy_load() {
    let root = tempfile::tempdir().expect("grant policy root");
    let path = root.path().join("grants.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "grants": [
                {"plugin_id": "plugin.alpha", "capability_id": "storage.plugin"},
                {"plugin_id": "plugin.alpha", "capability_id": "storage.plugin"}
            ]
        }))
        .expect("serialize grants"),
    )
    .expect("write grants");

    let (semantic, relational) = test_persistence();
    let semantic_snapshots = Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
    let error = PolicyHostCapabilityBroker::load(
        semantic,
        relational,
        Arc::new(std::sync::Mutex::new(None)),
        host_services(),
        semantic_snapshots,
        &path,
    )
    .err()
    .expect("duplicate rejected");

    assert!(error.to_string().contains("duplicate grant"));
}

#[test]
fn unknown_capability_is_rejected_during_policy_load() {
    let error = load_policy(json!({
        "schema_version": 1,
        "grants": [{
            "plugin_id": "plugin.alpha",
            "capability_id": "fabricated.capability"
        }]
    }))
    .err()
    .expect("unknown capability rejected");

    assert!(error.to_string().contains("fabricated.capability"));
    assert!(error.to_string().contains("registered Host Capability"));
}

#[test]
fn policy_cannot_claim_a_host_implementation_version() {
    let error = load_policy(json!({
        "schema_version": 1,
        "grants": [{
            "plugin_id": "plugin.alpha",
            "capability_id": "storage.plugin",
            "host_version": "9.0.0"
        }]
    }))
    .err()
    .expect("host-owned version field rejected");

    assert!(error.to_string().contains("host_version"));
    assert!(error.to_string().contains("unknown field"));
}

fn policy_broker(document: Value) -> PolicyHostCapabilityBroker {
    load_policy(document).expect("load grant policy")
}

fn load_policy(document: Value) -> Result<PolicyHostCapabilityBroker> {
    let root = tempfile::tempdir().expect("grant policy root");
    let path = root.path().join("grants.json");
    fs::write(
        &path,
        serde_json::to_vec(&document).expect("serialize grants"),
    )
    .expect("write grants");
    let (semantic, relational) = test_persistence();
    let semantic_snapshots = Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
    PolicyHostCapabilityBroker::load(
        semantic,
        relational,
        Arc::new(std::sync::Mutex::new(None)),
        host_services(),
        semantic_snapshots,
        &path,
    )
}

fn test_persistence() -> (
    Arc<dyn lumvise_db_core::SemanticPersistence>,
    Arc<dyn lumvise_db_core::RelationalPersistence>,
) {
    let persistence =
        Arc::new(lumvise_db_core::LocalPersistence::in_memory().expect("test persistence"));
    let semantic: Arc<dyn lumvise_db_core::SemanticPersistence> = persistence.clone();
    let relational: Arc<dyn lumvise_db_core::RelationalPersistence> = persistence;
    (semantic, relational)
}

fn host_services() -> Arc<PluginHostServices> {
    let persistence = test_persistence();
    PluginHostServices::new(
        lumvise_frontend_core::FrontendCore::default(),
        lumvise_neural_core::LlmProviderRegistry::empty(),
        persistence.0,
    )
}

fn storage_request(plugin_id: &str, required_version: &str) -> HostCapabilityRequest {
    HostCapabilityRequest {
        plugin_id: plugin_id.into(),
        invocation_id: "invocation-1".into(),
        call_id: "call-1".into(),
        capability_id: "storage.plugin".into(),
        required_version: required_version.into(),
        input: json!({
            "operation": "ensure_table",
            "table_name": "notes",
            "schema": {"type": "object"}
        }),
    }
}
