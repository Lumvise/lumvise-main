//! Production compiled-plugin bootstrap and Host Capability grant policy.
//!
//! App Core startup calls [`PluginProductionConfig`] once. Trust parsing,
//! permission gating, durable restore, and runtime construction remain hidden.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

use lumvise_plugin_package::{HostCompatibility, PluginReleaseIndexV2};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginInvocationContext,
    PluginRepository, PluginRepositoryConfig, PluginRestoreFailure, PluginRestoreFailureKind,
    PluginRuntimeConfig, PluginSystem,
};
use semver::VersionReq;
use serde::Deserialize;
use serde_json::Value;

use super::AppCoreHostCapabilityBroker;
use super::PluginHostServices;
use super::host_capability_broker::SharedPluginVectorizer;
use super::host_capability_catalog::host_capability_definition;
use crate::{AppCoreError, Result};
use tracing::error;

pub use lumvise_plugin_runtime::LUMVISE_PLUGIN_ROOT_ENV;

/// Distinct exports a built-in SDK plugin runs at once. Each export stays
/// serial by default; this bound only lets cheap workspace reads proceed
/// while a long export (scoped C4 generation, a background poll) is running.
const PRODUCTION_PLUGIN_PIPELINE_CAPACITY: u32 = 4;
const BUNDLED_RELEASE_DIR_ENV: &str = "LUMVISE_BUILTIN_RELEASE_DIR";

/// Explicit filesystem, compatibility, and process policy for production plugins.
#[derive(Clone, Debug)]
pub struct PluginProductionConfig {
    repository: PluginRepositoryConfig,
    host_capability_grants_path: PathBuf,
    runtime: PluginRuntimeConfig,
}

impl PluginProductionConfig {
    /// Creates production policy from explicit durable paths and host compatibility.
    ///
    /// # Example
    /// ```
    /// use lumvise_app_core::PluginProductionConfig;
    /// use lumvise_plugin_package::HostCompatibility;
    /// let config = PluginProductionConfig::new(
    ///     "/var/lib/lumvise/plugins",
    ///     "/etc/lumvise/publisher-trust.json",
    ///     "/etc/lumvise/host-capability-grants.json",
    ///     HostCompatibility::new(1, "x86_64-unknown-linux-gnu"),
    /// );
    /// assert!(config.repository_root().ends_with("plugins"));
    /// ```
    pub fn new(
        repository_root: impl Into<PathBuf>,
        publisher_trust_path: impl Into<PathBuf>,
        host_capability_grants_path: impl Into<PathBuf>,
        host: HostCompatibility,
    ) -> Self {
        Self {
            repository: PluginRepositoryConfig::new(repository_root, publisher_trust_path, host),
            host_capability_grants_path: host_capability_grants_path.into(),
            runtime: production_runtime_config(),
        }
    }

    /// Resolves standard per-user production paths and current Rust host target.
    pub fn configured() -> Self {
        let repository = PluginRepositoryConfig::configured();
        let host_capability_grants_path =
            repository.state_root().join("host-capability-grants.json");
        Self {
            repository,
            host_capability_grants_path,
            runtime: production_runtime_config(),
        }
    }

    /// Creates strict deny-all policy documents only when no policy exists yet.
    ///
    /// Existing documents are never overwritten. This supports first launch
    /// without silently replacing administrator-managed trust or grants.
    pub fn initialize_deny_all_policy_if_missing(&self) -> Result<()> {
        create_initial_policy(
            self.repository.publisher_trust_path(),
            br#"{"schema_version":1,"keys":[]}"#,
        )?;
        create_initial_policy(
            &self.host_capability_grants_path,
            br#"{"schema_version":1,"grants":[]}"#,
        )
    }

    /// Installs and enables a bundled signed release once per package version.
    pub(crate) fn install_bundled_release_if_present(&self) -> Result<()> {
        configured_bundled_release_dir()
            .map(|release_dir| self.install_bundled_release(&release_dir))
            .unwrap_or(Ok(()))
    }

    fn install_bundled_release(&self, release_dir: &Path) -> Result<()> {
        copy_initial_policy(
            &release_dir.join("publisher-trust.json"),
            self.repository.publisher_trust_path(),
        )?;
        copy_initial_policy(
            &release_dir.join("host-capability-grants.json"),
            &self.host_capability_grants_path,
        )?;
        let index = PluginReleaseIndexV2::read(release_dir.join("builtins-release.json"))?;
        index.verify_artifacts(release_dir)?;
        install_bundled_artifacts(&self.repository, release_dir, &index)
    }

    /// Replaces supervised process deadlines and framing limits.
    pub fn with_runtime_config(mut self, runtime: PluginRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }

    /// Returns durable repository root.
    pub fn repository_root(&self) -> &Path {
        self.repository.repository_root()
    }
}

fn production_runtime_config() -> PluginRuntimeConfig {
    let mut runtime = PluginRuntimeConfig::default();
    runtime.max_concurrent_invocations_per_plugin = PRODUCTION_PLUGIN_PIPELINE_CAPACITY;
    runtime
}

pub(crate) struct ProductionPluginBootstrap {
    pub(crate) system: Arc<PluginSystem>,
    pub(crate) repository: Arc<Mutex<PluginRepository>>,
    pub(crate) initial_restore: Arc<(Mutex<bool>, Condvar)>,
}

impl ProductionPluginBootstrap {
    pub(crate) fn open(
        config: PluginProductionConfig,
        semantic: Arc<dyn lumvise_db_core::SemanticPersistence>,
        relational: Arc<dyn lumvise_db_core::RelationalPersistence>,
        vectorizer: SharedPluginVectorizer,
        services: Arc<PluginHostServices>,
        semantic_snapshots: Arc<crate::SemanticSnapshotService>,
        registration_signal: Arc<(Mutex<u64>, Condvar)>,
    ) -> Result<Self> {
        let broker = Arc::new(PolicyHostCapabilityBroker::load(
            semantic,
            Arc::clone(&relational),
            vectorizer,
            Arc::clone(&services),
            semantic_snapshots,
            &config.host_capability_grants_path,
        )?);
        let system = Arc::new(PluginSystem::production_with_lanes(
            config.runtime,
            broker,
            services.exclusive_lanes(),
        )?);
        let repository = Arc::new(Mutex::new(config.repository.open()?));
        let initial_restore = Arc::new((Mutex::new(false), Condvar::new()));
        spawn_initial_restore(
            Arc::clone(&repository),
            Arc::clone(&system),
            relational,
            Arc::clone(&registration_signal),
            Arc::clone(&initial_restore),
        );
        Ok(Self {
            system,
            repository,
            initial_restore,
        })
    }
}

fn spawn_initial_restore(
    repository: Arc<Mutex<PluginRepository>>,
    system: Arc<PluginSystem>,
    relational: Arc<dyn lumvise_db_core::RelationalPersistence>,
    registration_signal: Arc<(Mutex<u64>, Condvar)>,
    initial_restore: Arc<(Mutex<bool>, Condvar)>,
) {
    std::thread::spawn(move || {
        if let Ok(mut repository) = repository.lock() {
            restore_installed_plugins(
                &mut repository,
                &system,
                relational.as_ref(),
                &registration_signal,
            );
        } else {
            log_plugin_bootstrap_error("bootstrap_lock", "plugin repository lock poisoned");
        }
        mark_initial_restore_complete(&initial_restore);
    });
}

fn mark_initial_restore_complete(initial_restore: &Arc<(Mutex<bool>, Condvar)>) {
    let (complete, notifier) = initial_restore.as_ref();
    if let Ok(mut complete) = complete.lock() {
        *complete = true;
        notifier.notify_all();
    } else {
        log_plugin_bootstrap_error("bootstrap_completion", "completion lock poisoned");
    }
}

fn restore_installed_plugins(
    repository: &mut PluginRepository,
    system: &Arc<PluginSystem>,
    relational: &dyn lumvise_db_core::RelationalPersistence,
    registration_signal: &Arc<(Mutex<u64>, Condvar)>,
) {
    let report = repository.restore_with_degradation_and_callback(system, |plugin_id, version| {
        publish_ready_plugin(system, relational, registration_signal, plugin_id, version);
    });
    match report {
        Ok(report) => {
            publish_restore_summary(system, relational, registration_signal, &report.degraded)
        }
        Err(error) => log_plugin_bootstrap_error("bootstrap_restore", &error.to_string()),
    }
}

fn publish_ready_plugin(
    system: &Arc<PluginSystem>,
    relational: &dyn lumvise_db_core::RelationalPersistence,
    registration_signal: &Arc<(Mutex<u64>, Condvar)>,
    plugin_id: &str,
    version: &str,
) {
    publish_background_registrations(relational, system, chrono::Utc::now().timestamp(), &[]);
    if let Err(error) = notify_background_registration_change(registration_signal) {
        log_plugin_bootstrap_error(
            "bootstrap_registration_signal",
            &format!("ready plugin `{plugin_id}`@`{version}`; {error}"),
        );
    }
}

fn publish_restore_summary(
    system: &Arc<PluginSystem>,
    relational: &dyn lumvise_db_core::RelationalPersistence,
    registration_signal: &Arc<(Mutex<u64>, Condvar)>,
    degraded: &[PluginRestoreFailure],
) {
    publish_background_registrations(relational, system, chrono::Utc::now().timestamp(), degraded);
    if let Err(error) = notify_background_registration_change(registration_signal) {
        log_plugin_bootstrap_error("bootstrap_registration_signal", &error.to_string());
    }
}

fn notify_background_registration_change(
    registration_signal: &Arc<(Mutex<u64>, Condvar)>,
) -> Result<()> {
    let (generation, notifier) = registration_signal.as_ref();
    let mut generation = generation
        .lock()
        .map_err(|_| AppCoreError::poisoned_mutex("recurring_registration_signal"))?;
    *generation = generation.saturating_add(1);
    notifier.notify_all();
    Ok(())
}

fn publish_background_registrations(
    relational: &dyn lumvise_db_core::RelationalPersistence,
    system: &PluginSystem,
    now_unix_seconds: i64,
    degraded: &[PluginRestoreFailure],
) {
    if !degraded.is_empty() {
        for failure in degraded {
            let disposition = match failure.kind {
                PluginRestoreFailureKind::PermanentAdmission => "disabled after admission failure",
                PluginRestoreFailureKind::TransientStartup => {
                    "enabled, inactive, retry scheduled by supervisor"
                }
            };
            log_plugin_bootstrap_error(
                "bootstrap_degraded_plugin",
                &format!(
                    "plugin `{}`@`{}` {}: {}",
                    failure.plugin_id, failure.version, disposition, failure.reason
                ),
            );
        }
    }
    match system.background_exports() {
        Ok(exports) => {
            if let Err(error) = crate::plugin::background_delivery::sync_background_registrations(
                relational,
                &exports,
                now_unix_seconds,
            ) {
                log_plugin_bootstrap_error("background_registration_sync", &error.to_string());
            }
        }
        Err(error) => log_plugin_bootstrap_error("background_exports", &error.to_string()),
    }
}

fn log_plugin_bootstrap_error(component: &str, error: &str) {
    error!(
        target: "app-core::plugin_bootstrap",
        component = component,
        message = %error,
        "plugin bootstrap failure"
    );
}

struct PolicyHostCapabilityBroker {
    grants: BTreeSet<(String, String)>,
    executor: AppCoreHostCapabilityBroker,
}

impl PolicyHostCapabilityBroker {
    fn load(
        semantic: Arc<dyn lumvise_db_core::SemanticPersistence>,
        relational: Arc<dyn lumvise_db_core::RelationalPersistence>,
        vectorizer: SharedPluginVectorizer,
        services: Arc<PluginHostServices>,
        semantic_snapshots: Arc<crate::SemanticSnapshotService>,
        path: &Path,
    ) -> Result<Self> {
        let bytes = fs::read(path).map_err(|error| invalid_policy(path, error.to_string()))?;
        let document: GrantDocument = serde_json::from_slice(&bytes)
            .map_err(|error| invalid_policy(path, error.to_string()))?;
        if document.schema_version != 1 {
            return Err(invalid_policy(
                path,
                format!("schema_version `{}`; expected `1`", document.schema_version),
            ));
        }
        let mut grants = BTreeSet::new();
        for record in document.grants {
            validate_policy_id(path, &record.plugin_id, "plugin_id")?;
            validate_policy_id(path, &record.capability_id, "capability_id")?;
            validate_registered_capability(path, &record.capability_id)?;
            let key = (record.plugin_id, record.capability_id);
            if !grants.insert(key.clone()) {
                return Err(invalid_policy(
                    path,
                    format!(
                        "duplicate grant `{}:{}`; expected one grant per plugin capability",
                        key.0, key.1
                    ),
                ));
            }
        }
        Ok(Self {
            grants,
            executor: AppCoreHostCapabilityBroker::with_services(
                semantic,
                relational,
                vectorizer,
                Some(services),
                semantic_snapshots,
            ),
        })
    }
}

impl HostCapabilityBroker for PolicyHostCapabilityBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> std::result::Result<Value, HostCapabilityError> {
        self.authorize(&request)?;
        self.executor.invoke(request)
    }

    fn invoke_controlled(
        &self,
        request: HostCapabilityRequest,
        context: &PluginInvocationContext,
    ) -> std::result::Result<Value, HostCapabilityError> {
        self.authorize(&request)?;
        self.executor.invoke_controlled(request, context)
    }
}

impl PolicyHostCapabilityBroker {
    fn authorize(
        &self,
        request: &HostCapabilityRequest,
    ) -> std::result::Result<(), HostCapabilityError> {
        let grant_key = (request.plugin_id.clone(), request.capability_id.clone());
        if !self.grants.contains(&grant_key) {
            return Err(HostCapabilityError::new(
                &request.capability_id,
                "host_capability_denied",
                format!(
                    "plugin `{}` has no grant for Host Capability `{}`",
                    request.plugin_id, request.capability_id
                ),
                false,
            ));
        }
        let host_version = host_capability_definition(&request.capability_id)
            .expect("grant loader accepts only catalog capabilities")
            .version();
        let required = VersionReq::parse(&request.required_version).map_err(|error| {
            HostCapabilityError::new(
                &request.capability_id,
                "invalid_host_capability_version",
                format!(
                    "signed requirement `{}` is invalid; expected semantic version requirement: {error}",
                    request.required_version
                ),
                false,
            )
        })?;
        if !required.matches(&host_version) {
            return Err(HostCapabilityError::new(
                &request.capability_id,
                "host_capability_version_mismatch",
                format!(
                    "host version `{host_version}` does not satisfy signed requirement `{required}`"
                ),
                false,
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantDocument {
    schema_version: u32,
    grants: Vec<GrantRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRecord {
    plugin_id: String,
    capability_id: String,
}

fn validate_registered_capability(path: &Path, capability_id: &str) -> Result<()> {
    if host_capability_definition(capability_id).is_some() {
        return Ok(());
    }
    Err(invalid_policy(
        path,
        format!(
            "capability_id `{capability_id}` is unknown; expected a registered Host Capability"
        ),
    ))
}

fn validate_policy_id(path: &Path, value: &str, field: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        });
    if valid {
        return Ok(());
    }
    Err(invalid_policy(
        path,
        format!("{field} `{value}` is invalid; expected non-empty lowercase ASCII stable identity"),
    ))
}

fn configured_bundled_release_dir() -> Option<PathBuf> {
    if let Some(directory) =
        std::env::var_os(BUNDLED_RELEASE_DIR_ENV).filter(|path| !path.is_empty())
    {
        return Some(PathBuf::from(directory));
    }
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    [
        directory.join("builtins"),
        directory.join("../Resources/builtins"),
    ]
    .into_iter()
    .find(|candidate| candidate.join("builtins-release.json").is_file())
}

fn install_bundled_artifacts(
    config: &PluginRepositoryConfig,
    release_dir: &Path,
    index: &PluginReleaseIndexV2,
) -> Result<()> {
    let mut repository = config.open()?;
    for artifact in &index.artifacts {
        let archive = release_dir.join(&artifact.archive_path);
        repository.install_bundled_archive(&archive)?;
    }
    Ok(())
}

fn copy_initial_policy(source: &Path, destination: &Path) -> Result<()> {
    let bytes = fs::read(source).map_err(|error| invalid_policy(source, error.to_string()))?;
    create_initial_policy(destination, &bytes)
}

fn invalid_policy(path: &Path, message: String) -> AppCoreError {
    AppCoreError::PluginProductionPolicy {
        path: path.to_owned(),
        message,
    }
}

fn create_initial_policy(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| invalid_policy(path, error.to_string()))?;
    }
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(invalid_policy(path, error.to_string())),
    };

    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| invalid_policy(path, error.to_string()))
}

#[cfg(test)]
mod tests {
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
                index["artifacts"][0]["archive_sha256"] =
                    json!(format!("{:x}", Sha256::digest(bytes)));
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

    fn test_production_config(state: PathBuf) -> PluginProductionConfig {
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

    fn test_manifest(plugin_id: &str, version: &str) -> PluginManifest {
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
        let semantic_snapshots =
            Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
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
        let semantic_snapshots =
            Arc::new(crate::SemanticSnapshotService::new(Arc::clone(&semantic)));
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
}
