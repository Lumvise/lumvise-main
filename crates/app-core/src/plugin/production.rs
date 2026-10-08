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

mod builtin_trust_upgrade;
use builtin_trust_upgrade::BuiltinTrustUpgrade;

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
    builtin_trust_upgrade: BuiltinTrustUpgrade,
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
            builtin_trust_upgrade: BuiltinTrustUpgrade::production(),
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
            builtin_trust_upgrade: BuiltinTrustUpgrade::production(),
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
        let index = PluginReleaseIndexV2::read(release_dir.join("builtins-release.json"))?;
        index.verify_artifacts(release_dir)?;
        self.builtin_trust_upgrade.prepare(
            release_dir,
            self.repository.publisher_trust_path(),
            &index,
        )?;
        copy_initial_policy(
            &release_dir.join("publisher-trust.json"),
            self.repository.publisher_trust_path(),
        )?;
        copy_initial_policy(
            &release_dir.join("host-capability-grants.json"),
            &self.host_capability_grants_path,
        )?;
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
mod tests;
