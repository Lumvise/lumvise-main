//! Deep managed-model lifecycle for local neural assets.
//!
//! The renderer and desktop bridge only see [`ManagedModelSnapshot`].  Source
//! URLs, temporary paths, archive layouts, integrity checks, and runtime
//! cutover stay behind this interface.  A selection is committed only after
//! download, validation, activation, and persistence all succeed.

use crate::{NeuralError, Result};
use bzip2::read::BzDecoder;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

/// Supported managed asset families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedModelKind {
    /// Semantic embedding model.
    Vector,
    /// Whisper speech-recognition model.
    SpeechToText,
    /// Kokoro text-to-speech bundle.
    TextToSpeech,
}

/// Catalog source asset and integrity metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSource {
    /// Production download URL.
    pub url: String,
    /// SHA-256 digest of the downloaded bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Expected byte count of the downloaded bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_bytes: Option<u64>,
    /// Destination relative to the published model directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    /// Whether the source is a bzip2 tar archive.
    #[serde(default)]
    pub tar_bz2: bool,
}

/// Catalog metadata shared by UI, storage, suitability, and activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModelCatalogEntry {
    /// Stable model identifier.
    pub id: String,
    /// Asset family.
    pub kind: ManagedModelKind,
    /// Human-readable name.
    pub display_name: String,
    /// Download size, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_size_bytes: Option<u64>,
    /// Production sources, tried in order.
    pub sources: Vec<ModelSource>,
    /// Required files/directories relative to the published model directory.
    pub installed_layout: Vec<String>,
    /// Supported operating systems (`macos`, `windows`, `linux`).
    pub operating_systems: Vec<String>,
    /// Supported target architectures (`aarch64`, `x86_64`).
    pub architectures: Vec<String>,
    /// Preferred acceleration backends.
    #[serde(default)]
    pub preferred_acceleration: Vec<String>,
    /// Acceleration backends that are required for execution.
    #[serde(default)]
    pub required_acceleration: Vec<String>,
    /// Minimum memory guidance in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_memory_mb: Option<u64>,
    /// Runtime adapter identifier.
    pub runtime_adapter: String,
    /// A valid non-download sentinel, used by the Disabled vector entry.
    #[serde(default)]
    pub disabled: bool,
}

/// Detected capabilities used for advisory suitability classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareFacts {
    /// Operating system name.
    pub operating_system: String,
    /// Target architecture.
    pub architecture: String,
    /// Logical CPU count.
    pub logical_cpus: u32,
    /// Total physical memory in MiB, when detectable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_memory_mb: Option<u64>,
    /// Available acceleration backends.
    #[serde(default)]
    pub acceleration: Vec<String>,
}

/// Suitability classification for one catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSuitability {
    /// Preferred capability and memory guidance are present.
    Recommended,
    /// Runtime fallback exists but performance/resource caveats apply.
    UsableWithCaveats,
    /// The runtime cannot execute the asset on this host.
    Unsupported,
}

/// Lifecycle state visible to the desktop bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedModelState {
    /// No validated asset is published.
    NotDownloaded,
    /// A shared download/validation/activation job is running.
    Downloading,
    /// The asset is validated and active/available.
    Ready,
    /// The latest replacement attempt failed; an earlier active model may remain.
    Failed,
}

/// User-safe status for one managed asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModelStatus {
    /// Stable catalog ID.
    pub model_id: String,
    /// Asset family.
    pub kind: ManagedModelKind,
    /// Display name.
    pub display_name: String,
    /// Lifecycle state.
    pub state: ManagedModelState,
    /// Bytes copied so far.
    pub downloaded_bytes: u64,
    /// Total bytes, when source metadata reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// Active committed model for this family.
    pub active: bool,
    /// Suitability classification.
    pub suitability: ModelSuitability,
    /// Explanation for suitability or failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// User-safe failure text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Snapshot delivered to the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedModelSnapshot {
    /// Hardware facts used by suitability explanations.
    pub hardware: HardwareFacts,
    /// Catalog plus current lifecycle state.
    pub models: Vec<ManagedModelStatus>,
}

/// Internal download adapter seam. Production uses [`HttpModelDownloader`];
/// deterministic tests inject a fake implementation.
pub trait ModelDownloader: Send + Sync {
    /// Downloads one source into `destination`, reporting byte progress.
    fn download(
        &self,
        source: &ModelSource,
        destination: &Path,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> std::result::Result<(), String>;
}

/// Internal hardware probe seam.
pub trait HardwareProbe: Send + Sync {
    /// Returns one stable capability snapshot.
    fn probe(&self) -> HardwareFacts;
}

/// Internal runtime activation/persistence seam.
///
/// `activate` must fully load the candidate into a private runtime instance;
/// only after it returns successfully does the manager call `persist`.  The
/// host adapter is responsible for swapping future work to that loaded
/// instance without interrupting in-flight work.
pub trait ModelRuntimeAdapter: Send + Sync {
    /// Loads a validated candidate without changing the committed selection.
    fn activate(
        &self,
        entry: &ManagedModelCatalogEntry,
        published_path: &Path,
    ) -> std::result::Result<(), String>;
    /// Persists the committed selection after activation succeeds.
    fn persist(&self, kind: ManagedModelKind, model_id: &str) -> std::result::Result<(), String>;
}

/// Production HTTP downloader.
#[derive(Debug, Clone)]
pub struct HttpModelDownloader {
    client: Client,
}

impl Default for HttpModelDownloader {
    fn default() -> Self {
        Self {
            client: Client::new(),
        }
    }
}

impl ModelDownloader for HttpModelDownloader {
    fn download(
        &self,
        source: &ModelSource,
        destination: &Path,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> std::result::Result<(), String> {
        let mut response = self
            .client
            .get(&source.url)
            .send()
            .map_err(|error| format!("model download failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("model download failed: {error}"))?;
        let total = response.content_length().or(source.expected_bytes);
        let mut file = File::create(destination)
            .map_err(|error| format!("creating model download file failed: {error}"))?;
        let mut downloaded = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("reading model download failed: {error}"))?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|error| format!("writing model download failed: {error}"))?;
            downloaded = downloaded.saturating_add(read as u64);
            progress(downloaded, total);
        }
        file.sync_all()
            .map_err(|error| format!("syncing model download failed: {error}"))?;
        Ok(())
    }
}

/// Production hardware probe using standard-library host facts and portable
/// operating-system memory APIs. Missing optional acceleration remains advisory.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeHardwareProbe;

impl HardwareProbe for RuntimeHardwareProbe {
    fn probe(&self) -> HardwareFacts {
        HardwareFacts {
            operating_system: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            logical_cpus: std::thread::available_parallelism()
                .map(|count| count.get() as u32)
                .unwrap_or(1),
            total_memory_mb: runtime_total_memory_mb(),
            acceleration: runtime_acceleration(),
        }
    }
}

fn runtime_total_memory_mb() -> Option<u64> {
    #[cfg(unix)]
    {
        let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if pages > 0 && page_size > 0 {
            return (pages as u128)
                .checked_mul(page_size as u128)
                .map(|bytes| (bytes / (1024 * 1024) as u128) as u64);
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..unsafe { std::mem::zeroed() }
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
            return Some(status.ullTotalPhys / (1024 * 1024));
        }
    }
    None
}

/// Runtime state root. Tests may inject `LUMVISE_STATE_ROOT`; production
/// resolves the user's home directory through the platform-aware `dirs` crate
/// and stores assets beneath `.lumvise/models`.
pub fn lumvise_state_root() -> PathBuf {
    if let Some(root) = std::env::var_os("LUMVISE_STATE_ROOT").filter(|value| !value.is_empty()) {
        return PathBuf::from(root);
    }
    dirs::home_dir()
        .map(|home| home.join(".lumvise"))
        .unwrap_or_else(|| PathBuf::from(".lumvise"))
}

/// Authoritative managed model directory beneath the Lumvise state root.
pub fn managed_models_root(state_root: &Path) -> PathBuf {
    state_root.join("models")
}

/// Lifecycle manager shared by App Core and deterministic desktop adapters.
#[derive(Clone)]
pub struct ManagedModelManager {
    inner: Arc<ManagedModelManagerInner>,
}

struct ManagedModelManagerInner {
    catalog: Vec<ManagedModelCatalogEntry>,
    state_root: PathBuf,
    hardware: HardwareFacts,
    downloader: Arc<dyn ModelDownloader>,
    runtime: Arc<dyn ModelRuntimeAdapter>,
    statuses: Mutex<BTreeMap<String, ManagedModelStatus>>,
    active: Mutex<HashMap<ManagedModelKind, String>>,
    jobs: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for ManagedModelManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedModelManager")
            .field("catalog_entries", &self.inner.catalog.len())
            .field("state_root", &self.inner.state_root)
            .finish()
    }
}

impl ManagedModelManager {
    /// Builds a manager from production adapters and validates local assets.
    pub fn production(runtime: Arc<dyn ModelRuntimeAdapter>) -> Result<Self> {
        Self::new(
            lumvise_state_root(),
            builtin_catalog(),
            Arc::new(HttpModelDownloader::default()),
            Arc::new(RuntimeHardwareProbe),
            runtime,
        )
    }

    /// Builds a manager with injected adapters. This is the deterministic test
    /// seam; callers still exercise the same catalog, lifecycle, and commit path.
    pub fn new(
        state_root: impl Into<PathBuf>,
        catalog: Vec<ManagedModelCatalogEntry>,
        downloader: Arc<dyn ModelDownloader>,
        probe: Arc<dyn HardwareProbe>,
        runtime: Arc<dyn ModelRuntimeAdapter>,
    ) -> Result<Self> {
        let state_root = state_root.into();
        fs::create_dir_all(managed_models_root(&state_root)).map_err(|source| NeuralError::Io {
            value: managed_models_root(&state_root).display().to_string(),
            expected: "writable Lumvise managed models directory".to_string(),
            source,
        })?;
        let hardware = probe.probe();
        let active = discover_active(&state_root, &catalog);
        let statuses = discover_statuses(&state_root, &catalog, &hardware, &active);
        Ok(Self {
            inner: Arc::new(ManagedModelManagerInner {
                catalog,
                state_root,
                hardware,
                downloader,
                runtime,
                statuses: Mutex::new(statuses),
                active: Mutex::new(active),
                jobs: Mutex::new(HashSet::new()),
            }),
        })
    }

    /// Returns catalog/status/hardware data without paths or secrets.
    pub fn snapshot(&self) -> ManagedModelSnapshot {
        let statuses = self
            .inner
            .statuses
            .lock()
            .map(|statuses| statuses.values().cloned().collect())
            .unwrap_or_default();
        ManagedModelSnapshot {
            hardware: self.inner.hardware.clone(),
            models: statuses,
        }
    }

    /// Returns the authoritative managed-model storage directory, so the
    /// Settings System category can display where assets live on disk.
    pub fn models_root(&self) -> PathBuf {
        managed_models_root(&self.inner.state_root)
    }

    /// Returns one catalog entry by stable ID.
    pub fn catalog_entry(&self, model_id: &str) -> Option<ManagedModelCatalogEntry> {
        self.inner
            .catalog
            .iter()
            .find(|entry| entry.id == model_id)
            .cloned()
    }

    /// Requests one model. Duplicate requests join the existing job.
    pub fn select(&self, model_id: &str) -> Result<ManagedModelStatus> {
        let entry = self
            .catalog_entry(model_id)
            .ok_or_else(|| NeuralError::InvalidValue {
                value: model_id.to_string(),
                expected: "known managed model id".to_string(),
            })?;
        let suitability = classify_suitability(&entry, &self.inner.hardware);
        if suitability.0 == ModelSuitability::Unsupported {
            return Err(NeuralError::ProviderFailed {
                provider_id: entry.id,
                message: suitability.1,
            });
        }

        // Hold the per-model job marker while deciding which lifecycle path to
        // take and while publishing the initial status. A second request can
        // therefore join an in-flight activation as well as a download; it
        // cannot reset progress or activate the same asset concurrently.
        let mut jobs = self
            .inner
            .jobs
            .lock()
            .map_err(|_| NeuralError::ProviderFailed {
                provider_id: "managed-models".to_string(),
                message: "managed model jobs mutex poisoned".to_string(),
            })?;
        if jobs.contains(&entry.id) {
            drop(jobs);
            return self.status_for(model_id);
        }
        jobs.insert(entry.id.clone());

        if entry.disabled {
            drop(jobs);
            let result = self.commit_disabled(&entry);
            self.finish_job(&entry.id);
            if let Err(error) = result {
                let message = error.to_string();
                self.fail(&entry, message);
                return Err(error);
            }
            return self.status_for(model_id);
        }

        let published = is_published(&self.inner.state_root, &entry);
        let active = self
            .inner
            .active
            .lock()
            .ok()
            .and_then(|active| active.get(&entry.kind).cloned());
        {
            let mut statuses = self.inner.statuses.lock().map_err(|_| {
                jobs.remove(&entry.id);
                NeuralError::ProviderFailed {
                    provider_id: "managed-models".to_string(),
                    message: "managed model status mutex poisoned".to_string(),
                }
            })?;
            let status = statuses
                .entry(entry.id.clone())
                .or_insert_with(|| ManagedModelStatus {
                    model_id: entry.id.clone(),
                    kind: entry.kind,
                    display_name: entry.display_name.clone(),
                    state: ManagedModelState::NotDownloaded,
                    downloaded_bytes: 0,
                    total_bytes: entry.download_size_bytes,
                    active: active.as_deref() == Some(entry.id.as_str()),
                    suitability: suitability.0.clone(),
                    explanation: Some(suitability.1.clone()),
                    failure: None,
                });
            status.total_bytes = entry.download_size_bytes;
            status.failure = None;
            status.active = active.as_deref() == Some(entry.id.as_str());
            if published {
                // Activation is a lifecycle operation even when no bytes need
                // downloading, so duplicate callers observe a non-ready state
                // until the runtime cutover has committed.
                status.state = ManagedModelState::Downloading;
            } else {
                status.state = ManagedModelState::Downloading;
                status.downloaded_bytes = 0;
            }
        }
        drop(jobs);

        if published {
            let result = self.activate_published(entry.clone());
            self.finish_job(&entry.id);
            if let Err(error) = result {
                return Err(error);
            }
            return self.status_for(model_id);
        }

        let manager = self.clone();
        let entry_for_job = entry.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("lumvise-model-{}", entry.id))
            .spawn(move || manager.download_and_commit(entry_for_job))
        {
            self.finish_job(model_id);
            let message = format!("starting model job failed: {error}");
            self.fail(&entry, message.clone());
            return Err(NeuralError::ProviderFailed {
                provider_id: "managed-models".to_string(),
                message,
            });
        }
        self.status_for(model_id)
    }

    /// Selects a model and waits for download, validation, activation, and
    /// persistence to finish. Desktop settings uses this when a selection
    /// operation has a synchronous runtime contract.
    pub fn select_blocking(&self, model_id: &str) -> Result<ManagedModelStatus> {
        self.select(model_id)?;
        loop {
            let status = self.status_for(model_id)?;
            if status.state != ManagedModelState::Downloading {
                if status.state == ManagedModelState::Failed {
                    return Err(NeuralError::ProviderFailed {
                        provider_id: model_id.to_string(),
                        message: status
                            .failure
                            .unwrap_or_else(|| "managed model selection failed".to_string()),
                    });
                }
                return Ok(status);
            }
            thread::yield_now();
        }
    }

    /// Retries a failed model using the same joining path.
    pub fn retry(&self, model_id: &str) -> Result<ManagedModelStatus> {
        self.select(model_id)
    }

    /// Reads one current status.
    pub fn status_for(&self, model_id: &str) -> Result<ManagedModelStatus> {
        self.inner
            .statuses
            .lock()
            .map_err(|_| NeuralError::ProviderFailed {
                provider_id: "managed-models".to_string(),
                message: "managed model status mutex poisoned".to_string(),
            })?
            .get(model_id)
            .cloned()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: model_id.to_string(),
                expected: "known managed model id".to_string(),
            })
    }

    fn finish_job(&self, model_id: &str) {
        if let Ok(mut jobs) = self.inner.jobs.lock() {
            jobs.remove(model_id);
        }
    }

    fn commit_disabled(&self, entry: &ManagedModelCatalogEntry) -> Result<()> {
        self.inner
            .runtime
            .activate(entry, &self.inner.state_root)
            .map_err(|message| NeuralError::ProviderFailed {
                provider_id: entry.id.clone(),
                message,
            })?;
        self.inner
            .runtime
            .persist(entry.kind, &entry.id)
            .map_err(|message| NeuralError::ProviderFailed {
                provider_id: entry.id.clone(),
                message,
            })?;
        self.commit_active(entry)
    }

    fn activate_published(&self, entry: ManagedModelCatalogEntry) -> Result<()> {
        let published = published_path(&self.inner.state_root, &entry);
        if let Err(message) = self.inner.runtime.activate(&entry, &published) {
            self.fail(&entry, message);
            return Err(NeuralError::ProviderFailed {
                provider_id: entry.id.clone(),
                message: "model activation failed; previous model remains active".to_string(),
            });
        }
        if let Err(message) = self.inner.runtime.persist(entry.kind, &entry.id) {
            self.fail(&entry, message);
            return Err(NeuralError::ProviderFailed {
                provider_id: entry.id.clone(),
                message: "model persistence failed; previous model remains active".to_string(),
            });
        }
        self.commit_active(&entry)
    }

    fn download_and_commit(&self, entry: ManagedModelCatalogEntry) {
        let result = self.download_entry(&entry);
        if let Err(message) = result {
            self.fail(&entry, message);
        }
        self.finish_job(&entry.id);
    }

    fn download_entry(&self, entry: &ManagedModelCatalogEntry) -> std::result::Result<(), String> {
        if entry.sources.is_empty() {
            return Err("managed model has no production source".to_string());
        }
        let models_root = managed_models_root(&self.inner.state_root);
        let final_path = published_path(&self.inner.state_root, entry);
        let staging = models_root.join(format!(".{}.staging-{}", entry.id, uuid::Uuid::new_v4()));
        fs::create_dir_all(&staging)
            .map_err(|error| format!("creating model staging directory failed: {error}"))?;
        let published_staging = staging.join("published");
        fs::create_dir_all(&published_staging).map_err(|error| {
            format!("creating published model staging directory failed: {error}")
        })?;
        let result = (|| {
            for (index, source) in entry.sources.iter().enumerate() {
                let download_path = staging.join(format!("source-{index}.asset"));
                let mut update_progress = |bytes: u64, total: Option<u64>| {
                    if let Ok(mut statuses) = self.inner.statuses.lock()
                        && let Some(status) = statuses.get_mut(&entry.id)
                    {
                        status.downloaded_bytes = bytes;
                        status.total_bytes = total.or(status.total_bytes);
                        status.state = ManagedModelState::Downloading;
                    }
                };
                self.inner
                    .downloader
                    .download(source, &download_path, &mut update_progress)?;
                validate_integrity(&download_path, source)?;
                if source.tar_bz2 {
                    extract_tar_bz2(&download_path, &published_staging)?;
                } else {
                    let relative = source.relative_path.as_deref().unwrap_or("model.asset");
                    let destination = safe_join(&published_staging, relative)?;
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent).map_err(|error| {
                            format!("creating model asset parent failed: {error}")
                        })?;
                    }
                    fs::rename(&download_path, destination)
                        .map_err(|error| format!("staging model asset failed: {error}"))?;
                }
            }
            validate_layout(&published_staging, &entry.installed_layout)?;
            if final_path.exists() {
                fs::remove_dir_all(&final_path)
                    .map_err(|error| format!("replacing model publication failed: {error}"))?;
            }
            fs::rename(&published_staging, &final_path)
                .map_err(|error| format!("atomic model publication failed: {error}"))?;
            if let Err(message) = self.inner.runtime.activate(entry, &final_path) {
                return Err(format!(
                    "model activation failed; previous model remains active: {message}"
                ));
            }
            if let Err(message) = self.inner.runtime.persist(entry.kind, &entry.id) {
                return Err(format!(
                    "model persistence failed; previous model remains active: {message}"
                ));
            }
            self.commit_active(entry).map_err(|error| error.to_string())
        })();
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn commit_active(&self, entry: &ManagedModelCatalogEntry) -> Result<()> {
        let previous = {
            let mut active = self
                .inner
                .active
                .lock()
                .map_err(|_| NeuralError::ProviderFailed {
                    provider_id: "managed-models".to_string(),
                    message: "managed model active mutex poisoned".to_string(),
                })?;
            let previous = active.get(&entry.kind).cloned();
            let mut snapshot = active.clone();
            snapshot.insert(entry.kind, entry.id.clone());
            write_active_selections(&self.inner.state_root, &snapshot).map_err(|message| {
                NeuralError::ProviderFailed {
                    provider_id: entry.id.clone(),
                    message: format!("persisting managed model selection failed: {message}"),
                }
            })?;
            active.insert(entry.kind, entry.id.clone());
            previous
        };
        let mut statuses = self
            .inner
            .statuses
            .lock()
            .map_err(|_| NeuralError::ProviderFailed {
                provider_id: "managed-models".to_string(),
                message: "managed model status mutex poisoned".to_string(),
            })?;
        for status in statuses
            .values_mut()
            .filter(|status| status.kind == entry.kind)
        {
            status.active = status.model_id == entry.id;
            if status.model_id == entry.id {
                status.state = ManagedModelState::Ready;
                status.failure = None;
                status.explanation = Some("validated and active for new work".to_string());
            } else if previous.as_deref() == Some(status.model_id.as_str()) {
                status.active = false;
            }
        }
        Ok(())
    }

    /// Reloads and reactivates every persisted active model at startup, so a
    /// restart restores the running vector/speech runtime rather than only
    /// the status text. A failure activating one persisted model is recorded
    /// as a failed, inactive status and its active marker is removed; storage
    /// and manager errors remain fatal.
    pub fn restore_active_selections(&self) -> std::result::Result<(), String> {
        let active_selections: Vec<(ManagedModelKind, String)> = self
            .inner
            .active
            .lock()
            .map_err(|_| "managed model active mutex poisoned".to_string())?
            .iter()
            .map(|(kind, model_id)| (*kind, model_id.clone()))
            .collect();
        for (kind, model_id) in active_selections {
            let Some(entry) = self.catalog_entry(&model_id) else {
                continue;
            };
            if entry.kind != kind {
                continue;
            }
            let activation_path = if entry.disabled {
                self.inner.state_root.clone()
            } else {
                published_path(&self.inner.state_root, &entry)
            };
            self.restore_active_selection(&entry, &activation_path)?;
        }
        Ok(())
    }

    fn restore_active_selection(
        &self,
        entry: &ManagedModelCatalogEntry,
        activation_path: &Path,
    ) -> std::result::Result<(), String> {
        if let Err(message) = self.inner.runtime.activate(entry, activation_path) {
            self.clear_active_selection(entry)?;
            self.fail(entry, message);
            return Ok(());
        }
        if let Err(message) = self.inner.runtime.persist(entry.kind, &entry.id) {
            self.fail(entry, message);
            return Err(NeuralError::ProviderFailed {
                provider_id: entry.id.clone(),
                message: "model persistence failed; previous model remains active".to_string(),
            }
            .to_string());
        }
        self.commit_active(entry).map_err(|error| error.to_string())
    }

    fn clear_active_selection(
        &self,
        entry: &ManagedModelCatalogEntry,
    ) -> std::result::Result<(), String> {
        let mut active = self
            .inner
            .active
            .lock()
            .map_err(|_| "managed model active mutex poisoned".to_string())?;
        if active.get(&entry.kind) != Some(&entry.id) {
            return Ok(());
        }
        let mut snapshot = active.clone();
        snapshot.remove(&entry.kind);
        write_active_selections(&self.inner.state_root, &snapshot)?;
        active.remove(&entry.kind);
        Ok(())
    }

    fn fail(&self, entry: &ManagedModelCatalogEntry, message: String) {
        if let Ok(active) = self.inner.active.lock()
            && let Ok(mut statuses) = self.inner.statuses.lock()
            && let Some(status) = statuses.get_mut(&entry.id)
        {
            status.state = ManagedModelState::Failed;
            status.failure = Some(message);
            status.active = active.get(&entry.kind).is_some_and(|id| id == &entry.id);
        }
    }
}

fn discover_active(
    state_root: &Path,
    catalog: &[ManagedModelCatalogEntry],
) -> HashMap<ManagedModelKind, String> {
    read_active_selections(state_root)
        .into_iter()
        .filter(|(kind, model_id)| {
            catalog.iter().any(|entry| {
                &entry.id == model_id
                    && &entry.kind == kind
                    && (entry.disabled || is_published(state_root, entry))
            })
        })
        .collect()
}

fn active_selections_path(state_root: &Path) -> PathBuf {
    managed_models_root(state_root).join(".active-selections.json")
}

fn read_active_selections(state_root: &Path) -> HashMap<ManagedModelKind, String> {
    let Ok(contents) = fs::read_to_string(active_selections_path(state_root)) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<ManagedModelKind, String>>(&contents).unwrap_or_default()
}

fn write_active_selections(
    state_root: &Path,
    active: &HashMap<ManagedModelKind, String>,
) -> std::result::Result<(), String> {
    let path = active_selections_path(state_root);
    let payload = serde_json::to_vec(active).map_err(|error| {
        format!("serializing managed model active-selection marker failed: {error}")
    })?;
    let temp = managed_models_root(state_root)
        .join(format!(".active-selections-{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut file = File::create(&temp).map_err(|error| {
            format!("writing managed model active-selection marker failed: {error}")
        })?;
        file.write_all(&payload).map_err(|error| {
            format!("writing managed model active-selection marker failed: {error}")
        })?;
        file.sync_all().map_err(|error| {
            format!("syncing managed model active-selection marker failed: {error}")
        })?;
        Ok::<(), String>(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }

    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path).map_err(|error| {
            format!("replacing managed model active-selection marker failed: {error}")
        })?;
    }
    let result = fs::rename(&temp, &path).map_err(|error| {
        format!("publishing managed model active-selection marker failed: {error}")
    });
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn discover_statuses(
    state_root: &Path,
    catalog: &[ManagedModelCatalogEntry],
    hardware: &HardwareFacts,
    active: &HashMap<ManagedModelKind, String>,
) -> BTreeMap<String, ManagedModelStatus> {
    catalog
        .iter()
        .map(|entry| {
            let (suitability, explanation) = classify_suitability(entry, hardware);
            let published = entry.disabled || is_published(state_root, entry);
            (
                entry.id.clone(),
                ManagedModelStatus {
                    model_id: entry.id.clone(),
                    kind: entry.kind,
                    display_name: entry.display_name.clone(),
                    state: if published {
                        ManagedModelState::Ready
                    } else {
                        ManagedModelState::NotDownloaded
                    },
                    downloaded_bytes: if published {
                        entry.download_size_bytes.unwrap_or(0)
                    } else {
                        0
                    },
                    total_bytes: entry.download_size_bytes,
                    active: active.get(&entry.kind) == Some(&entry.id),
                    suitability,
                    explanation: Some(explanation),
                    failure: None,
                },
            )
        })
        .collect()
}

fn classify_suitability(
    entry: &ManagedModelCatalogEntry,
    hardware: &HardwareFacts,
) -> (ModelSuitability, String) {
    if entry.disabled {
        return (
            ModelSuitability::Recommended,
            "Vector generation is explicitly disabled; lexical fallback remains available."
                .to_string(),
        );
    }
    if !entry
        .operating_systems
        .iter()
        .any(|os| os == &hardware.operating_system)
        || !entry
            .architectures
            .iter()
            .any(|arch| arch == &hardware.architecture)
    {
        return (
            ModelSuitability::Unsupported,
            format!(
                "requires one of {:?} on {:?}; detected {} / {}",
                entry.operating_systems,
                entry.architectures,
                hardware.operating_system,
                hardware.architecture
            ),
        );
    }
    if let Some(minimum) = entry.minimum_memory_mb
        && let Some(total) = hardware.total_memory_mb
        && total < minimum
    {
        return (
            ModelSuitability::Unsupported,
            format!("requires at least {minimum} MiB; detected {total} MiB"),
        );
    }
    if entry.required_acceleration.iter().any(|required| {
        !hardware
            .acceleration
            .iter()
            .any(|available| available == required)
    }) {
        return (
            ModelSuitability::Unsupported,
            format!("requires acceleration {:?}", entry.required_acceleration),
        );
    }
    if entry.preferred_acceleration.iter().any(|preferred| {
        hardware
            .acceleration
            .iter()
            .any(|available| available == preferred)
    }) {
        return (
            ModelSuitability::Recommended,
            "preferred acceleration is available; CPU fallback remains supported.".to_string(),
        );
    }
    (
        ModelSuitability::UsableWithCaveats,
        "supported through a CPU/runtime fallback; performance may be lower without preferred acceleration.".to_string(),
    )
}

fn is_published(state_root: &Path, entry: &ManagedModelCatalogEntry) -> bool {
    if entry.disabled {
        return true;
    }
    let path = published_path(state_root, entry);
    path.is_dir() && validate_layout(&path, &entry.installed_layout).is_ok()
}

fn published_path(state_root: &Path, entry: &ManagedModelCatalogEntry) -> PathBuf {
    managed_models_root(state_root).join(&entry.id)
}

fn validate_integrity(path: &Path, source: &ModelSource) -> std::result::Result<(), String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("model source metadata failed: {error}"))?;
    if let Some(expected) = source.expected_bytes
        && metadata.len() != expected
    {
        return Err(format!(
            "model source size was {}, expected {expected}",
            metadata.len()
        ));
    }
    if let Some(expected) = source.sha256.as_deref() {
        let mut file =
            File::open(path).map_err(|error| format!("model integrity read failed: {error}"))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("model integrity read failed: {error}"))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let actual = hex::encode(digest.finalize());
        if actual != expected {
            return Err(format!(
                "model integrity mismatch: got {actual}, expected {expected}"
            ));
        }
    }
    Ok(())
}

fn validate_layout(root: &Path, layout: &[String]) -> std::result::Result<(), String> {
    for relative in layout {
        let path = safe_join(root, relative)?;
        let metadata = fs::metadata(&path).map_err(|error| {
            format!("required model asset `{relative}` is unavailable: {error}")
        })?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(format!(
                "required model asset `{relative}` is not a file or directory"
            ));
        }
    }
    Ok(())
}

fn extract_tar_bz2(source: &Path, destination: &Path) -> std::result::Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("creating archive staging directory failed: {error}"))?;
    let file =
        File::open(source).map_err(|error| format!("opening model archive failed: {error}"))?;
    let decoder = BzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("reading model archive failed: {error}"))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("reading model archive entry failed: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("reading model archive path failed: {error}"))?
            .into_owned();
        let safe = safe_join(destination, &path.to_string_lossy())?;
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&safe)
                .map_err(|error| format!("creating model archive directory failed: {error}"))?;
        } else if entry.header().entry_type().is_file() {
            if let Some(parent) = safe.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("creating model archive parent failed: {error}"))?;
            }
            entry
                .unpack(&safe)
                .map_err(|error| format!("extracting model archive entry failed: {error}"))?;
        } else {
            return Err(format!("unsupported model archive entry `{path:?}`"));
        }
    }
    Ok(())
}

fn safe_join(root: &Path, relative: &str) -> std::result::Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(format!("archive path `{relative}` is absolute"));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "archive path `{relative}` is not a relative model path"
        ));
    }
    Ok(root.join(path))
}

fn runtime_acceleration() -> Vec<String> {
    let mut values = Vec::new();
    #[cfg(target_os = "macos")]
    values.push("metal".to_string());
    if let Some(value) = std::env::var_os("LUMVISE_ACCELERATION") {
        values.extend(value.to_string_lossy().split(',').filter_map(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        }));
    }
    values
}

/// Root-plus-onnx file set every FastEmbed ONNX mirror (Xenova conversions)
/// publishes: tokenizer/config metadata alongside the quantized weights.
fn fastembed_sources(repo: &str) -> Vec<ModelSource> {
    [
        ("config.json", "config.json"),
        ("special_tokens_map.json", "special_tokens_map.json"),
        ("tokenizer.json", "tokenizer.json"),
        ("tokenizer_config.json", "tokenizer_config.json"),
        ("onnx/model_quantized.onnx", "model.onnx"),
    ]
    .into_iter()
    .map(|(remote, local)| ModelSource {
        url: format!("https://huggingface.co/{repo}/resolve/main/{remote}"),
        sha256: None,
        expected_bytes: None,
        relative_path: Some(local.to_string()),
        tar_bz2: false,
    })
    .collect()
}

fn fastembed_layout() -> Vec<String> {
    vec![
        "config.json".into(),
        "special_tokens_map.json".into(),
        "tokenizer.json".into(),
        "tokenizer_config.json".into(),
        "model.onnx".into(),
    ]
}

/// Built-in production catalog. URLs are source assets, never renderer input.
pub fn builtin_catalog() -> Vec<ManagedModelCatalogEntry> {
    let platforms = vec![
        "macos".to_string(),
        "windows".to_string(),
        "linux".to_string(),
    ];
    let architectures = vec!["aarch64".to_string(), "x86_64".to_string()];
    vec![
        ManagedModelCatalogEntry {
            id: "vector.disabled".into(),
            kind: ManagedModelKind::Vector,
            display_name: "Disabled (lexical fallback)".into(),
            download_size_bytes: None,
            sources: Vec::new(),
            installed_layout: Vec::new(),
            operating_systems: platforms.clone(),
            architectures: architectures.clone(),
            preferred_acceleration: Vec::new(),
            required_acceleration: Vec::new(),
            minimum_memory_mb: None,
            runtime_adapter: "vector.disabled".into(),
            disabled: true,
        },
        ManagedModelCatalogEntry {
            id: "vector.all-minilm-l6-v2".into(),
            kind: ManagedModelKind::Vector,
            display_name: "AllMiniLM-L6-v2".into(),
            download_size_bytes: None,
            sources: fastembed_sources("Xenova/all-MiniLM-L6-v2"),
            installed_layout: fastembed_layout(),
            operating_systems: platforms.clone(),
            architectures: architectures.clone(),
            preferred_acceleration: vec!["metal".into()],
            required_acceleration: Vec::new(),
            minimum_memory_mb: Some(512),
            runtime_adapter: "fastembed.all-minilm-l6-v2".into(),
            disabled: false,
        },
        ManagedModelCatalogEntry {
            id: "vector.bge-small-en-v1.5".into(),
            kind: ManagedModelKind::Vector,
            display_name: "BGE Small EN v1.5".into(),
            download_size_bytes: None,
            sources: fastembed_sources("Xenova/bge-small-en-v1.5"),
            installed_layout: fastembed_layout(),
            operating_systems: platforms.clone(),
            architectures: architectures.clone(),
            preferred_acceleration: vec!["metal".into()],
            required_acceleration: Vec::new(),
            minimum_memory_mb: Some(768),
            runtime_adapter: "fastembed.bge-small-en-v1.5".into(),
            disabled: false,
        },
        ManagedModelCatalogEntry {
            id: "vector.bge-m3".into(),
            kind: ManagedModelKind::Vector,
            display_name: "BGE-M3".into(),
            download_size_bytes: None,
            sources: fastembed_sources("Xenova/bge-m3"),
            installed_layout: fastembed_layout(),
            operating_systems: platforms.clone(),
            architectures: architectures.clone(),
            preferred_acceleration: vec!["metal".into()],
            required_acceleration: Vec::new(),
            minimum_memory_mb: Some(4096),
            runtime_adapter: "fastembed.bge-m3".into(),
            disabled: false,
        },
        whisper_entry("stt.whisper-tiny-en", "Whisper tiny.en", "ggml-tiny.en.bin"),
        whisper_entry("stt.whisper-base-en", "Whisper base.en", "ggml-base.en.bin"),
        whisper_entry("stt.whisper-small-en", "Whisper small.en", "ggml-small.en.bin"),
        whisper_entry("stt.whisper-large-v3-turbo", "Whisper large-v3-turbo", "ggml-large-v3-turbo.bin"),
        ManagedModelCatalogEntry {
            id: "tts.kokoro".into(),
            kind: ManagedModelKind::TextToSpeech,
            display_name: "Kokoro (complete English bundle)".into(),
            download_size_bytes: None,
            sources: vec![ModelSource {
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-en-v0_19.tar.bz2".into(),
                sha256: None,
                expected_bytes: None,
                relative_path: None,
                tar_bz2: true,
            }],
            installed_layout: vec![
                "kokoro-en-v0_19/model.onnx".into(),
                "kokoro-en-v0_19/voices.bin".into(),
                "kokoro-en-v0_19/tokens.txt".into(),
                "kokoro-en-v0_19/espeak-ng-data".into(),
            ],
            operating_systems: platforms,
            architectures,
            preferred_acceleration: Vec::new(),
            required_acceleration: Vec::new(),
            minimum_memory_mb: Some(2048),
            runtime_adapter: "kokoro.sherpa-onnx".into(),
            disabled: false,
        },
    ]
}

fn whisper_entry(id: &str, display_name: &str, file_name: &str) -> ManagedModelCatalogEntry {
    let platforms = vec![
        "macos".to_string(),
        "windows".to_string(),
        "linux".to_string(),
    ];
    let architectures = vec!["aarch64".to_string(), "x86_64".to_string()];
    ManagedModelCatalogEntry {
        id: id.into(),
        kind: ManagedModelKind::SpeechToText,
        display_name: display_name.into(),
        download_size_bytes: None,
        sources: vec![ModelSource {
            url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file_name}"),
            sha256: None,
            expected_bytes: None,
            relative_path: Some("model.asset".to_string()),
            tar_bz2: false,
        }],
        installed_layout: vec!["model.asset".into()],
        operating_systems: platforms,
        architectures,
        preferred_acceleration: vec!["metal".into()],
        required_acceleration: Vec::new(),
        minimum_memory_mb: Some(if file_name.contains("large") {
            4096
        } else {
            512
        }),
        runtime_adapter: "whisper-rs".into(),
        disabled: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[derive(Debug)]
    struct FakeDownloader {
        bytes: Vec<u8>,
        calls: Arc<Mutex<u32>>,
    }

    impl ModelDownloader for FakeDownloader {
        fn download(
            &self,
            source: &ModelSource,
            destination: &Path,
            progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> std::result::Result<(), String> {
            let _ = source;
            *self.calls.lock().unwrap() += 1;
            let mut file = File::create(destination).map_err(|error| error.to_string())?;
            file.write_all(&self.bytes)
                .map_err(|error| error.to_string())?;
            progress(self.bytes.len() as u64, Some(self.bytes.len() as u64));
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FakeProbe(HardwareFacts);
    impl HardwareProbe for FakeProbe {
        fn probe(&self) -> HardwareFacts {
            self.0.clone()
        }
    }

    #[derive(Debug, Default)]
    struct FakeRuntime {
        activated: Mutex<Vec<String>>,
        persisted: Mutex<Vec<String>>,
    }
    impl ModelRuntimeAdapter for FakeRuntime {
        fn activate(
            &self,
            entry: &ManagedModelCatalogEntry,
            _path: &Path,
        ) -> std::result::Result<(), String> {
            self.activated.lock().unwrap().push(entry.id.clone());
            Ok(())
        }
        fn persist(
            &self,
            _kind: ManagedModelKind,
            model_id: &str,
        ) -> std::result::Result<(), String> {
            self.persisted.lock().unwrap().push(model_id.to_string());
            Ok(())
        }
    }
    #[derive(Debug)]
    struct FailingRestoreRuntime {
        failing_model: String,
    }

    impl ModelRuntimeAdapter for FailingRestoreRuntime {
        fn activate(
            &self,
            entry: &ManagedModelCatalogEntry,
            _path: &Path,
        ) -> std::result::Result<(), String> {
            if entry.id == self.failing_model {
                Err(format!(
                    "provider `{}` failed: model activation failed; previous model remains active",
                    entry.id
                ))
            } else {
                Ok(())
            }
        }

        fn persist(
            &self,
            _kind: ManagedModelKind,
            _model_id: &str,
        ) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    fn entry_of_kind(id: &str, kind: ManagedModelKind) -> ManagedModelCatalogEntry {
        let mut model = entry(id);
        model.kind = kind;
        model
    }

    #[test]
    fn restoring_failed_persisted_selection_is_recoverable_and_clears_marker() {
        let root = tempdir().unwrap();
        let catalog = vec![
            entry_of_kind("old", ManagedModelKind::Vector),
            entry_of_kind("failed", ManagedModelKind::SpeechToText),
        ];
        let setup = ManagedModelManager::new(
            root.path(),
            catalog.clone(),
            Arc::new(FakeDownloader {
                bytes: b"abc".to_vec(),
                calls: Arc::new(Mutex::new(0)),
            }),
            Arc::new(FakeProbe(HardwareFacts {
                operating_system: "macos".into(),
                architecture: "aarch64".into(),
                logical_cpus: 8,
                total_memory_mb: Some(16_384),
                acceleration: vec!["metal".into()],
            })),
            Arc::new(FakeRuntime::default()),
        )
        .unwrap();
        setup.select_blocking("old").unwrap();
        setup.select_blocking("failed").unwrap();

        let restoring = ManagedModelManager::new(
            root.path(),
            catalog.clone(),
            Arc::new(FakeDownloader {
                bytes: b"abc".to_vec(),
                calls: Arc::new(Mutex::new(0)),
            }),
            Arc::new(FakeProbe(HardwareFacts {
                operating_system: "macos".into(),
                architecture: "aarch64".into(),
                logical_cpus: 8,
                total_memory_mb: Some(16_384),
                acceleration: vec!["metal".into()],
            })),
            Arc::new(FailingRestoreRuntime {
                failing_model: "failed".into(),
            }),
        )
        .unwrap();

        restoring.restore_active_selections().unwrap();

        let old_status = restoring.status_for("old").unwrap();
        assert_eq!(old_status.state, ManagedModelState::Ready);
        assert!(old_status.active);

        let failed_status = restoring.status_for("failed").unwrap();
        assert_eq!(failed_status.state, ManagedModelState::Failed);
        assert!(!failed_status.active);
        assert_eq!(
            failed_status.failure.as_deref(),
            Some(
                "provider `failed` failed: model activation failed; previous model remains active"
            )
        );

        let reopened = ManagedModelManager::new(
            root.path(),
            catalog,
            Arc::new(FakeDownloader {
                bytes: b"abc".to_vec(),
                calls: Arc::new(Mutex::new(0)),
            }),
            Arc::new(FakeProbe(HardwareFacts {
                operating_system: "macos".into(),
                architecture: "aarch64".into(),
                logical_cpus: 8,
                total_memory_mb: Some(16_384),
                acceleration: vec!["metal".into()],
            })),
            Arc::new(FakeRuntime::default()),
        )
        .unwrap();
        assert!(reopened.status_for("old").unwrap().active);
        assert!(!reopened.status_for("failed").unwrap().active);
    }

    fn entry(id: &str) -> ManagedModelCatalogEntry {
        ManagedModelCatalogEntry {
            id: id.into(),
            kind: ManagedModelKind::Vector,
            display_name: id.into(),
            download_size_bytes: Some(3),
            sources: vec![ModelSource {
                url: "fixture://model".into(),
                sha256: None,
                expected_bytes: Some(3),
                relative_path: Some("model.asset".into()),
                tar_bz2: false,
            }],
            installed_layout: vec!["model.asset".into()],
            operating_systems: vec!["macos".into()],
            architectures: vec!["aarch64".into()],
            preferred_acceleration: vec!["metal".into()],
            required_acceleration: Vec::new(),
            minimum_memory_mb: Some(1),
            runtime_adapter: "fixture".into(),
            disabled: false,
        }
    }

    #[test]
    fn failed_replacement_preserves_previous_active_status() {
        let root = tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        let manager = ManagedModelManager::new(
            root.path(),
            vec![entry("old"), entry("new")],
            Arc::new(FakeDownloader {
                bytes: b"abc".to_vec(),
                calls: Arc::new(Mutex::new(0)),
            }),
            Arc::new(FakeProbe(HardwareFacts {
                operating_system: "macos".into(),
                architecture: "aarch64".into(),
                logical_cpus: 8,
                total_memory_mb: Some(16_384),
                acceleration: vec!["metal".into()],
            })),
            runtime,
        )
        .unwrap();
        manager.select("old").unwrap();
        while matches!(
            manager.status_for("old").unwrap().state,
            ManagedModelState::Downloading
        ) {
            std::thread::yield_now();
        }
        assert_eq!(
            manager.status_for("old").unwrap().state,
            ManagedModelState::Ready
        );
        assert!(manager.select("new").is_ok());
    }

    #[test]
    fn unsupported_model_is_rejected_before_download() {
        let root = tempdir().unwrap();
        let manager = ManagedModelManager::new(
            root.path(),
            vec![entry("windows-only")],
            Arc::new(FakeDownloader {
                bytes: b"abc".to_vec(),
                calls: Arc::new(Mutex::new(0)),
            }),
            Arc::new(FakeProbe(HardwareFacts {
                operating_system: "linux".into(),
                architecture: "x86_64".into(),
                logical_cpus: 4,
                total_memory_mb: Some(512),
                acceleration: Vec::new(),
            })),
            Arc::new(FakeRuntime::default()),
        )
        .unwrap();
        assert!(manager.select("windows-only").is_err());
    }

    #[test]
    fn safe_join_rejects_archive_escape() {
        assert!(safe_join(Path::new("/tmp/models"), "../outside").is_err());
        assert!(safe_join(Path::new("/tmp/models"), "/absolute").is_err());
    }

    #[test]
    fn safe_join_accepts_nested_relative_archive_path() {
        let root = Path::new("/tmp/models");
        assert_eq!(
            safe_join(root, "nested/model.bin").unwrap(),
            root.join("nested/model.bin")
        );
    }

    #[cfg(windows)]
    #[test]
    fn safe_join_rejects_windows_rooted_and_prefixed_archive_paths() {
        let root = Path::new(r"C:\models");
        for relative in [
            r"\absolute",
            r"C:relative",
            r"C:\absolute",
            r"\\server\share\absolute",
        ] {
            let error = safe_join(root, relative).unwrap_err();
            assert!(
                error.contains(relative),
                "error for `{relative}` omitted offending archive path: {error}"
            );
        }
    }
}
