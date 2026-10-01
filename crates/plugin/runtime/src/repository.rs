use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
    sync::mpsc,
    thread,
};

use lumvise_plugin_package::{
    HostCompatibility, InstalledPlugin, PluginManifest, PublisherTrustStore,
    install_verified_package,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use zip::ZipArchive;

use crate::{PluginRuntimeError, PluginSystem};

mod activation;
mod staging;
mod version_policy;
use staging::{StagedRemoval, remove_tree, rollback_archive, rollback_install, stage_archive};
use version_policy::{check_selection, parse_version, record_selection, should_enable_bundle};

const SCHEMA_VERSION: u32 = 1;

/// One durable compiled-plugin package version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginRegistryRecord {
    /// Signed plugin identity.
    pub plugin_id: String,
    /// Signed semantic version.
    pub version: String,
    /// Signed canonical manifest digest.
    pub digest: String,
    /// Content-addressed archive path relative to the repository.
    pub archive_path: String,
    /// Immutable extraction path relative to the repository.
    pub extracted_path: String,
    /// Whether this is the selected active version.
    pub enabled: bool,
    /// Signed publisher identity.
    pub publisher_id: String,
    /// Signed publisher key identity.
    pub key_id: String,
}

/// Durable registry snapshot persisted atomically as JSON.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginRegistry {
    /// Registry schema version.
    pub schema_version: u32,
    /// Installed versions in deterministic identity/version order.
    pub records: Vec<PluginRegistryRecord>,
    /// Highest version ever durably selected for each plugin.
    #[serde(default)]
    pub highest_enabled_versions: BTreeMap<String, String>,
}

/// Typed classification of a single plugin restore failure.
///
/// Classification is determined by lifecycle phase, not error text: the
/// background supervisor only retries [`PluginSystem::start`], so a failure is
/// retryable only when the plugin has already entered the runtime catalog.
/// Only [`PluginRestoreFailureKind::PermanentAdmission`] mutations clear
/// durable enabled intent; transient failures leave the record enabled and
/// cataloged-but-inactive for supervisor retry while remaining unpublished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginRestoreFailureKind {
    /// The durable record is disabled because no later supervisor tick can
    /// repair it.
    ///
    /// Covers every failure before or during runtime catalog admission:
    /// archive/registry re-verification and metadata mismatch, package
    /// admission rejection (signature, hash tree, manifest, compatibility,
    /// target, publisher/key/trust), repository or package I/O, static schema
    /// compilation, and install rejection. In all these cases the plugin never
    /// enters the catalog, so the supervisor's later `start` would only
    /// rediscover `NotInstalled`.
    PermanentAdmission,
    /// The durable record stays enabled because a later supervisor tick can
    /// repair it.
    ///
    /// Covers a process start failure after a successful install: spawn or
    /// sandbox availability, child exit, handshake timeout, protocol or
    /// ready/session identity failure, and concurrent lifecycle race. The
    /// plugin remains cataloged but inactive, which is exactly the state the
    /// supervisor's periodic `start` retries.
    TransientStartup,
}

/// Per-plugin restore result when startup is non-fatal and degradable.
#[derive(Debug, Eq, PartialEq)]
pub struct PluginRestoreFailure {
    /// Plugin identity that failed to restart from its archive.
    pub plugin_id: String,
    /// Enabled plugin version attempted during startup.
    pub version: String,
    /// Whether this failure durably disables the record or leaves it retryable.
    pub kind: PluginRestoreFailureKind,
    /// Structured reason produced by restore or runtime start.
    pub reason: String,
}

impl PluginRestoreFailure {
    fn new(
        plugin_id: impl Into<String>,
        version: impl Into<String>,
        kind: PluginRestoreFailureKind,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            version: version.into(),
            kind,
            reason: reason.into(),
        }
    }
}

/// Non-fatal startup restore outcome for a production plugin repository.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct PluginRestoreReport {
    /// Plugins that became active during restore.
    pub started: Vec<String>,
    /// Plugins that did not become ready. Transient members remain enabled and
    /// are retried by the supervisor; permanent members are durably disabled.
    pub degraded: Vec<PluginRestoreFailure>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            records: Vec::new(),
            highest_enabled_versions: BTreeMap::new(),
        }
    }
}

/// Filesystem removal policy for an uninstalled version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallPolicy {
    /// Retain the content-addressed archive cache.
    RetainArchive,
    /// Purge an archive when no registry record references it.
    Purge,
}

/// Durable package repository failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginRepositoryError {
    /// Repository filesystem I/O failed.
    #[error("plugin repository I/O failed for `{path}`: {source}")]
    Io {
        /// Offending path.
        path: PathBuf,
        /// Operating-system error.
        source: std::io::Error,
    },
    /// Registry JSON or invariant is invalid.
    #[error("invalid plugin registry `{path}`: {message}")]
    InvalidRegistry {
        /// Registry or archive path.
        path: PathBuf,
        /// Parse or invariant diagnostic.
        message: String,
    },
    /// Official package verification or extraction failed.
    #[error("plugin package operation failed: {0}")]
    Package(#[from] lumvise_plugin_package::PackageError),
    /// Runtime restore failed.
    #[error("plugin runtime restore failed: {0}")]
    Runtime(#[from] PluginRuntimeError),
    /// Exact signed version already exists.
    #[error("plugin `{plugin_id}` version `{version}` digest `{digest}` is already registered")]
    DuplicateVersion {
        /// Plugin identity.
        plugin_id: String,
        /// Semantic version.
        version: String,
        /// Existing digest.
        digest: String,
    },
    /// Same version points to different signed content.
    #[error(
        "plugin `{plugin_id}` version `{version}` digest collision: `{existing}` versus `{actual}`"
    )]
    VersionCollision {
        /// Plugin identity.
        plugin_id: String,
        /// Semantic version.
        version: String,
        /// Existing digest.
        existing: String,
        /// New digest.
        actual: String,
    },
    /// Candidate runtime was ready, but the caller's final validation failed.
    #[error("plugin `{plugin_id}` version `{version}` activation validation failed: {message}")]
    ActivationValidation {
        /// Plugin identity.
        plugin_id: String,
        /// Candidate semantic version.
        version: String,
        /// Caller-owned catalog validation failure.
        message: String,
    },
    /// Requested registry record is absent.
    #[error("plugin `{plugin_id}` version `{version}` is not registered")]
    NotRegistered {
        /// Plugin identity.
        plugin_id: String,
        /// Semantic version.
        version: String,
    },
    /// Reverified signed metadata differs from durable state.
    #[error(
        "stored plugin `{plugin_id}` version `{version}` {field} mismatch: expected `{expected}`, got `{actual}`"
    )]
    MetadataMismatch {
        /// Plugin identity.
        plugin_id: String,
        /// Semantic version.
        version: String,
        /// Mismatched field.
        field: &'static str,
        /// Registry value.
        expected: String,
        /// Reverified value.
        actual: String,
    },
}

/// Runtime-owned durable repository for signed compiled plugin packages.
pub struct PluginRepository {
    root: PathBuf,
    trust: Arc<PublisherTrustStore>,
    host: HostCompatibility,
    registry: PluginRegistry,
}

impl PluginRepository {
    /// Opens or creates a repository and validates its registry.
    pub fn open(
        root: impl Into<PathBuf>,
        trust: Arc<PublisherTrustStore>,
        host: HostCompatibility,
    ) -> Result<Self, PluginRepositoryError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| io_error(&root, source))?;
        let mut registry = load_registry(&root)?;
        validate_registry(&root, &registry)?;
        let mut seeded = false;
        for record in registry.records.iter().filter(|record| record.enabled) {
            if !registry
                .highest_enabled_versions
                .contains_key(&record.plugin_id)
            {
                registry
                    .highest_enabled_versions
                    .insert(record.plugin_id.clone(), record.version.clone());
                seeded = true;
            }
        }
        if seeded {
            save_registry(&root, &registry)?;
        }
        Ok(Self {
            root,
            trust,
            host,
            registry,
        })
    }

    /// Returns the current durable snapshot.
    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    /// Verifies, content-addresses, extracts, and atomically registers an archive.
    pub fn install_archive(
        &mut self,
        source: &Path,
        enabled: bool,
    ) -> Result<PluginRegistryRecord, PluginRepositoryError> {
        let verified = self.trust.verify_package(source, &self.host)?;
        let manifest = read_manifest(source)?;
        check_collision(&self.registry, &manifest, &manifest_digest(&manifest)?)?;
        parse_version(&manifest.plugin_version).map_err(|message| invalid(source, message))?;
        if enabled {
            check_selection(
                &self.registry,
                &manifest.plugin_id,
                &manifest.plugin_version,
            )
            .map_err(|message| invalid(source, message))?;
        }
        let archive_relative = format!("archives/{}.lvp", digest_file(source)?);
        let archive = self.root.join(&archive_relative);
        let archive_created = stage_archive(source, &archive)?;
        let installed = install_verified_package(&verified, &self.root.join("extracted"))
            .inspect_err(|_| rollback_archive(archive_created, &archive))?;
        let mut record = record(&manifest, &installed, archive_relative, &self.root)?;
        let mut next = self.registry.clone();
        if let Err(error) = insert(&mut next, &mut record, enabled) {
            rollback_install(&installed, archive_created, &archive);
            return Err(error);
        }
        if let Err(error) = save_registry(&self.root, &next) {
            rollback_install(&installed, archive_created, &archive);
            return Err(error);
        }
        self.registry = next;
        Ok(record)
    }

    /// Installs a signed bundled archive, selecting only a first or newer release.
    ///
    /// Example: `repository.install_bundled_archive(Path::new("release/plugin.lvp"))?`.
    pub fn install_bundled_archive(
        &mut self,
        source: &Path,
    ) -> Result<PluginRegistryRecord, PluginRepositoryError> {
        self.trust.verify_package(source, &self.host)?;
        let manifest = read_manifest(source)?;
        let digest = manifest_digest(&manifest)?;
        if let Some(existing) = self.registry.records.iter().find(|record| {
            record.plugin_id == manifest.plugin_id && record.version == manifest.plugin_version
        }) {
            if existing.digest != digest {
                return Err(PluginRepositoryError::VersionCollision {
                    plugin_id: manifest.plugin_id,
                    version: manifest.plugin_version,
                    existing: existing.digest.clone(),
                    actual: digest,
                });
            }
            return Ok(existing.clone());
        }
        let enabled = should_enable_bundle(
            &self.registry,
            &manifest.plugin_id,
            &manifest.plugin_version,
        )
        .map_err(|message| invalid(source, message))?;
        self.install_archive(source, enabled)
    }

    /// Selects exactly one registered version as active.
    pub fn enable(&mut self, plugin_id: &str, version: &str) -> Result<(), PluginRepositoryError> {
        let mut next = self.registry.clone();
        let found = next
            .records
            .iter()
            .any(|record| record.plugin_id == plugin_id && record.version == version);
        if !found {
            return Err(not_registered(plugin_id, version));
        }
        record_selection(&mut next, plugin_id, version)
            .map_err(|message| invalid(&self.root, message))?;
        self.commit(next)
    }

    /// Disables every registered version of a plugin.
    pub fn disable(&mut self, plugin_id: &str) -> Result<(), PluginRepositoryError> {
        let mut next = self.registry.clone();
        next.records
            .iter_mut()
            .filter(|record| record.plugin_id == plugin_id)
            .for_each(|record| record.enabled = false);
        self.commit(next)
    }

    /// Stops and durably disables one plugin as one rollback-safe transition.
    pub fn deactivate(
        &mut self,
        system: &PluginSystem,
        plugin_id: &str,
    ) -> Result<(), PluginRepositoryError> {
        let previous = self
            .registry
            .records
            .iter()
            .find(|record| record.plugin_id == plugin_id && record.enabled)
            .cloned();
        let previous_package = previous
            .as_ref()
            .map(|record| self.prepare_restore(record))
            .transpose()?;
        if let Err(error) = detach_runtime(system, plugin_id) {
            let _ = restore_previous_runtime(system, previous_package.as_ref());
            return Err(error);
        }
        let mut next = self.registry.clone();
        next.records
            .iter_mut()
            .filter(|record| record.plugin_id == plugin_id)
            .for_each(|record| record.enabled = false);
        if let Err(error) = self.commit(next) {
            let _ = restore_previous_runtime(system, previous_package.as_ref());
            return Err(error);
        }
        Ok(())
    }

    fn commit(&mut self, next: PluginRegistry) -> Result<(), PluginRepositoryError> {
        save_registry(&self.root, &next)?;
        self.registry = next;
        Ok(())
    }

    /// Removes registry and extracted state using explicit archive policy.
    pub fn uninstall(
        &mut self,
        plugin_id: &str,
        version: &str,
        policy: UninstallPolicy,
    ) -> Result<(), PluginRepositoryError> {
        let Some(index) = self
            .registry
            .records
            .iter()
            .position(|record| record.plugin_id == plugin_id && record.version == version)
        else {
            return Err(not_registered(plugin_id, version));
        };
        let mut next = self.registry.clone();
        let removed = next.records.remove(index);
        let purge_archive = policy == UninstallPolicy::Purge
            && !next
                .records
                .iter()
                .any(|record| record.archive_path == removed.archive_path);
        let staged = StagedRemoval::stage(&self.root, &removed, purge_archive)?;
        if let Err(error) = self.commit(next) {
            staged.rollback()?;
            return Err(error);
        }
        staged.finish();
        Ok(())
    }

    /// Reverifies and restores every enabled archive into a plugin system.
    pub fn restore_into(&self, system: &PluginSystem) -> Result<(), PluginRepositoryError> {
        let prepared: Vec<_> = self
            .registry
            .records
            .iter()
            .filter(|record| record.enabled)
            .map(|record| self.prepare_restore(record))
            .collect::<Result<_, _>>()?;
        publish_restored(system, &prepared)
    }

    /// Restores enabled plugins, disabling any plugin that cannot be reverified or
    /// restarted while continuing remaining plugins.
    pub fn restore_with_degradation(
        &mut self,
        system: &PluginSystem,
    ) -> Result<PluginRestoreReport, PluginRepositoryError> {
        self.restore_with_degradation_and_callback(system, |_, _| {})
    }

    /// Restores enabled plugins and invokes `on_started` for each successful one.
    ///
    /// The callback receives plugin id and version so callers can publish
    /// readiness changes as soon as an individual runtime reaches ready state.
    pub fn restore_with_degradation_and_callback<F>(
        &mut self,
        system: &PluginSystem,
        mut on_started: F,
    ) -> Result<PluginRestoreReport, PluginRepositoryError>
    where
        F: FnMut(&str, &str),
    {
        let enabled_plugins = self
            .registry
            .records
            .iter()
            .filter(|record| record.enabled)
            .cloned()
            .collect::<Vec<_>>();
        let mut report = PluginRestoreReport::default();
        let repository = &*self;
        let (sender, receiver) = mpsc::channel();
        thread::scope(|scope| {
            for record in enabled_plugins {
                let sender = sender.clone();
                scope.spawn(move || {
                    let plugin_id = record.plugin_id.clone();
                    let version = record.version.clone();
                    let failure = restore_record_with_classification(
                        repository, system, &record, &plugin_id, &version,
                    );
                    let _ = sender.send((plugin_id, version, failure));
                });
            }
            drop(sender);
            for (plugin_id, version, failure) in receiver {
                match failure {
                    None => {
                        on_started(&plugin_id, &version);
                        report.started.push(plugin_id);
                    }
                    Some(failure) => report.degraded.push(failure),
                }
            }
        });
        self.disable_records_for_failures(&report.degraded)?;
        report.started.sort();
        Ok(report)
    }

    fn disable_records_for_failures(
        &mut self,
        failures: &[PluginRestoreFailure],
    ) -> Result<(), PluginRepositoryError> {
        let has_permanent = failures
            .iter()
            .any(|failure| failure.kind == PluginRestoreFailureKind::PermanentAdmission);
        if !has_permanent {
            return Ok(());
        }
        let mut next = self.registry.clone();
        for failure in failures
            .iter()
            .filter(|failure| failure.kind == PluginRestoreFailureKind::PermanentAdmission)
        {
            for record in next.records.iter_mut() {
                if record.plugin_id == failure.plugin_id && record.version == failure.version {
                    record.enabled = false;
                }
            }
        }
        self.commit(next)
    }

    fn prepare_restore(
        &self,
        record: &PluginRegistryRecord,
    ) -> Result<InstalledPlugin, PluginRepositoryError> {
        let archive = safe_join(&self.root, &record.archive_path)?;
        let expected_archive_digest = archive
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid(&archive, "expected content-addressed archive name".into()))?;
        compare(
            record,
            "archive_digest",
            expected_archive_digest,
            &digest_file(&archive)?,
        )?;
        let verified = self.trust.verify_package(&archive, &self.host)?;
        validate_manifest(record, &read_manifest(&archive)?)?;
        remove_tree(&self.root.join(&record.extracted_path))?;
        let installed = install_verified_package(&verified, &self.root.join("extracted"))?;
        compare(record, "digest", &record.digest, installed.package_digest())?;
        Ok(installed)
    }
}

fn attach_runtime(
    system: &PluginSystem,
    package: &InstalledPlugin,
) -> Result<(), PluginRepositoryError> {
    system.install(package)?;
    if let Err(error) = system.start(package.plugin_id()) {
        let _ = system.uninstall(package.plugin_id());
        return Err(error.into());
    }
    Ok(())
}

/// Restores one record, classifying failure by lifecycle phase.
///
/// Classification is by phase, never by error text, because the background
/// supervisor only retries [`PluginSystem::start`]. A failure before or during
/// [`PluginSystem::install`] never catalogs the plugin, so the supervisor's
/// later `start` would only rediscover `NotInstalled`; those failures are
/// durably disabled. Only a `start` failure leaves the plugin cataloged but
/// inactive, which is the state the supervisor can repair, so it stays
/// enabled and retryable. A start-failed plugin is deliberately left installed
/// so its identity stays fail-closed instead of admitting a static fallback.
fn restore_record_with_classification(
    repository: &PluginRepository,
    system: &PluginSystem,
    record: &PluginRegistryRecord,
    plugin_id: &str,
    version: &str,
) -> Option<PluginRestoreFailure> {
    let permanent = |reason: PluginRepositoryError| {
        Some(PluginRestoreFailure::new(
            plugin_id,
            version,
            PluginRestoreFailureKind::PermanentAdmission,
            reason.to_string(),
        ))
    };
    // Phase: prepare. The plugin never reaches the runtime catalog, so a later
    // supervisor start cannot repair it.
    let package = match repository.prepare_restore(record) {
        Ok(package) => package,
        Err(error) => return permanent(error),
    };
    // Phase: catalog admission. A failed install leaves no catalog entry for
    // the supervisor to retry.
    if let Err(error) = system.install(&package) {
        return permanent(PluginRepositoryError::from(error));
    }
    // Phase: process start. A failure here leaves the plugin cataloged but
    // inactive; the supervisor's system.start retries it, so the durable
    // desired state stays enabled.
    system.start(package.plugin_id()).err().map(|error| {
        PluginRestoreFailure::new(
            plugin_id,
            version,
            PluginRestoreFailureKind::TransientStartup,
            PluginRepositoryError::from(error).to_string(),
        )
    })
}

fn detach_runtime(system: &PluginSystem, plugin_id: &str) -> Result<(), PluginRepositoryError> {
    if !system.is_installed(plugin_id)? {
        return Ok(());
    }
    let stop_error = if system.is_active(plugin_id)? {
        system.stop(plugin_id).err()
    } else {
        None
    };
    system.uninstall(plugin_id)?;
    if let Some(error) = stop_error {
        return Err(error.into());
    }
    Ok(())
}

fn restore_previous_runtime(
    system: &PluginSystem,
    previous: Option<&InstalledPlugin>,
) -> Result<(), PluginRepositoryError> {
    let Some(previous) = previous else {
        return Ok(());
    };
    if system.is_active(previous.plugin_id())? {
        return Ok(());
    }
    detach_runtime(system, previous.plugin_id())?;
    attach_runtime(system, previous)
}

fn publish_restored(
    system: &PluginSystem,
    installed: &[InstalledPlugin],
) -> Result<(), PluginRepositoryError> {
    let mut published = Vec::new();
    for package in installed {
        if let Err(error) = system
            .install(package)
            .and_then(|()| system.start(package.plugin_id()))
        {
            rollback_published(system, &published, package.plugin_id());
            return Err(error.into());
        }
        published.push(package.plugin_id().to_owned());
    }
    Ok(())
}

fn rollback_published(system: &PluginSystem, published: &[String], current: &str) {
    let _ = system.stop(current);
    let _ = system.uninstall(current);
    for plugin_id in published.iter().rev() {
        let _ = system.stop(plugin_id);
        let _ = system.uninstall(plugin_id);
    }
}

fn load_registry(root: &Path) -> Result<PluginRegistry, PluginRepositoryError> {
    let path = root.join("registry.json");
    if !path.exists() {
        return Ok(PluginRegistry::default());
    }
    let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(&path, error.to_string()))
}

fn save_registry(root: &Path, registry: &PluginRegistry) -> Result<(), PluginRepositoryError> {
    let path = root.join("registry.json");
    let bytes =
        serde_json::to_vec_pretty(registry).map_err(|error| invalid(&path, error.to_string()))?;
    let mut staged = NamedTempFile::new_in(root).map_err(|source| io_error(root, source))?;
    staged
        .write_all(&bytes)
        .map_err(|source| io_error(&path, source))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|source| io_error(&path, source))?;
    staged
        .persist(&path)
        .map_err(|error| io_error(&path, error.error))?;
    Ok(())
}

fn validate_registry(root: &Path, registry: &PluginRegistry) -> Result<(), PluginRepositoryError> {
    if registry.schema_version != SCHEMA_VERSION {
        return Err(invalid(
            root,
            format!(
                "schema_version `{}`; expected `{SCHEMA_VERSION}`",
                registry.schema_version
            ),
        ));
    }
    let mut versions = HashSet::new();
    let mut enabled = HashSet::new();
    for record in &registry.records {
        parse_version(&record.version).map_err(|message| invalid(root, message))?;
        if record.enabled {
            check_selection(registry, &record.plugin_id, &record.version)
                .map_err(|message| invalid(root, message))?;
        }
        if !versions.insert((&record.plugin_id, &record.version)) {
            return Err(invalid(
                root,
                format!("duplicate `{}` `{}`", record.plugin_id, record.version),
            ));
        }
        if record.enabled && !enabled.insert(&record.plugin_id) {
            return Err(invalid(
                root,
                format!("multiple enabled versions for `{}`", record.plugin_id),
            ));
        }
        validate_relative(root, &record.archive_path)?;
        validate_relative(root, &record.extracted_path)?;
        if !record.archive_path.starts_with("archives/") {
            return Err(invalid(
                root,
                format!(
                    "archive path `{}`; expected `archives/` prefix",
                    record.archive_path
                ),
            ));
        }
        if !record.extracted_path.starts_with("extracted/") {
            return Err(invalid(
                root,
                format!(
                    "extracted path `{}`; expected `extracted/` prefix",
                    record.extracted_path
                ),
            ));
        }
    }
    for (plugin_id, floor) in &registry.highest_enabled_versions {
        parse_version(floor)
            .map_err(|message| invalid(root, format!("plugin `{plugin_id}` {message}")))?;
    }
    Ok(())
}

fn insert(
    registry: &mut PluginRegistry,
    record: &mut PluginRegistryRecord,
    enabled: bool,
) -> Result<(), PluginRepositoryError> {
    if let Some(existing) = registry.records.iter().find(|existing| {
        existing.plugin_id == record.plugin_id && existing.version == record.version
    }) {
        if existing.digest == record.digest {
            return Err(PluginRepositoryError::DuplicateVersion {
                plugin_id: record.plugin_id.clone(),
                version: record.version.clone(),
                digest: record.digest.clone(),
            });
        }
        return Err(PluginRepositoryError::VersionCollision {
            plugin_id: record.plugin_id.clone(),
            version: record.version.clone(),
            existing: existing.digest.clone(),
            actual: record.digest.clone(),
        });
    }
    if enabled {
        record_selection(registry, &record.plugin_id, &record.version)
            .map_err(|message| invalid(Path::new("registry.json"), message))?;
    }
    record.enabled = enabled;
    registry.records.push(record.clone());
    registry
        .records
        .sort_by(|a, b| (&a.plugin_id, &a.version).cmp(&(&b.plugin_id, &b.version)));
    Ok(())
}

fn check_collision(
    registry: &PluginRegistry,
    manifest: &PluginManifest,
    digest: &str,
) -> Result<(), PluginRepositoryError> {
    let Some(existing) = registry.records.iter().find(|record| {
        record.plugin_id == manifest.plugin_id && record.version == manifest.plugin_version
    }) else {
        return Ok(());
    };
    if existing.digest == digest {
        return Err(PluginRepositoryError::DuplicateVersion {
            plugin_id: manifest.plugin_id.clone(),
            version: manifest.plugin_version.clone(),
            digest: digest.to_owned(),
        });
    }
    Err(PluginRepositoryError::VersionCollision {
        plugin_id: manifest.plugin_id.clone(),
        version: manifest.plugin_version.clone(),
        existing: existing.digest.clone(),
        actual: digest.to_owned(),
    })
}

fn manifest_digest(manifest: &PluginManifest) -> Result<String, PluginRepositoryError> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| invalid(Path::new("manifest.json"), error.to_string()))?;
    Ok(hex_digest(&Sha256::digest(bytes)))
}

fn read_manifest(path: &Path) -> Result<PluginManifest, PluginRepositoryError> {
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut archive = ZipArchive::new(file).map_err(|error| invalid(path, error.to_string()))?;
    let mut manifest = archive
        .by_name("manifest.json")
        .map_err(|error| invalid(path, error.to_string()))?;
    let mut bytes = Vec::new();
    manifest
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(path, error.to_string()))
}

fn record(
    manifest: &PluginManifest,
    installed: &InstalledPlugin,
    archive_path: String,
    root: &Path,
) -> Result<PluginRegistryRecord, PluginRepositoryError> {
    let extracted = installed
        .root()
        .strip_prefix(root)
        .map_err(|_| invalid(root, "extraction escaped repository".into()))?;
    Ok(PluginRegistryRecord {
        plugin_id: installed.plugin_id().into(),
        version: installed.plugin_version().into(),
        digest: installed.package_digest().into(),
        archive_path,
        extracted_path: path_text(extracted)?,
        enabled: false,
        publisher_id: manifest.publisher.publisher_id.clone(),
        key_id: manifest.publisher.key_id.clone(),
    })
}

fn validate_manifest(
    record: &PluginRegistryRecord,
    manifest: &PluginManifest,
) -> Result<(), PluginRepositoryError> {
    compare(record, "plugin_id", &record.plugin_id, &manifest.plugin_id)?;
    compare(record, "version", &record.version, &manifest.plugin_version)?;
    compare(
        record,
        "publisher_id",
        &record.publisher_id,
        &manifest.publisher.publisher_id,
    )?;
    compare(record, "key_id", &record.key_id, &manifest.publisher.key_id)
}

fn compare(
    record: &PluginRegistryRecord,
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), PluginRepositoryError> {
    if expected == actual {
        return Ok(());
    }
    Err(PluginRepositoryError::MetadataMismatch {
        plugin_id: record.plugin_id.clone(),
        version: record.version.clone(),
        field,
        expected: expected.into(),
        actual: actual.into(),
    })
}

fn digest_file(path: &Path) -> Result<String, PluginRepositoryError> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    Ok(hex_digest(&Sha256::digest(bytes)))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, PluginRepositoryError> {
    validate_relative(root, relative)?;
    Ok(root.join(relative))
}

fn validate_relative(root: &Path, relative: &str) -> Result<(), PluginRepositoryError> {
    let path = Path::new(relative);
    let safe = !relative.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|item| matches!(item, Component::Normal(_)));
    if safe {
        return Ok(());
    }
    Err(invalid(root, format!("unsafe relative path `{relative}`")))
}

fn path_text(path: &Path) -> Result<String, PluginRepositoryError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid(path, "expected UTF-8 path".into()))
}

fn not_registered(plugin_id: &str, version: &str) -> PluginRepositoryError {
    PluginRepositoryError::NotRegistered {
        plugin_id: plugin_id.into(),
        version: version.into(),
    }
}

fn io_error(path: &Path, source: std::io::Error) -> PluginRepositoryError {
    PluginRepositoryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn invalid(path: &Path, message: String) -> PluginRepositoryError {
    PluginRepositoryError::InvalidRegistry {
        path: path.to_path_buf(),
        message,
    }
}
