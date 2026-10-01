use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const DISCOVERY_SCHEMA_VERSION: u32 = 2;
pub(crate) const DISCOVERY_FILE: &str = "runtime.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeDiscovery {
    #[serde(rename = "schemaVersion")]
    pub(crate) schema_version: u32,
    #[serde(rename = "runtimeName")]
    pub(crate) runtime_name: String,
    #[serde(rename = "generationNonce")]
    pub(crate) generation_nonce: String,
    #[serde(rename = "controlEndpoint")]
    pub(crate) control_endpoint: String,
    #[serde(rename = "appBridgeBaseUrl")]
    pub(crate) app_bridge_base_url: Option<String>,
    pub(crate) state: String,
    pub(crate) pid: u32,
    #[serde(rename = "processStart")]
    pub(crate) process_start: String,
}

pub(crate) fn default_root() -> PathBuf {
    if let Some(root) = std::env::var_os("LUMVISE_RUNTIME_ROOT") {
        return PathBuf::from(root);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Application Support/Lumvise/runtime");
    }
    #[cfg(target_os = "windows")]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("Lumvise").join("runtime");
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("lumvise");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumvise")
        .join("runtime")
}

pub(crate) fn discovery_path(root: &Path) -> PathBuf {
    root.join(DISCOVERY_FILE)
}

pub(crate) fn read(path: &Path) -> Option<RuntimeDiscovery> {
    let body = fs::read(path).ok()?;
    let record = serde_json::from_slice::<RuntimeDiscovery>(&body).ok()?;
    if record.schema_version != DISCOVERY_SCHEMA_VERSION
        || record.runtime_name != "lumvise"
        || record.generation_nonce.trim().is_empty()
        || record.control_endpoint.trim().is_empty()
        || record.pid == 0
        || !matches!(
            record.state.as_str(),
            "starting" | "ready" | "quitting" | "exited"
        )
    {
        return None;
    }
    Some(record)
}

pub(crate) fn publish(path: &Path, record: &RuntimeDiscovery) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let body = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    fs::write(&temporary, body)?;
    fs::rename(temporary, path)
}

pub(crate) fn clear_if_generation(path: &Path, generation_nonce: &str) -> std::io::Result<()> {
    let Some(record) = read(path) else {
        return Ok(());
    };
    if record.generation_nonce == generation_nonce {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        Ok(())
    }
}
