//! Configures and opens the runtime-owned Plugin Package repository.
//!
//! Callers provide explicit paths or use the canonical per-user layout. Trust
//! loading, host compatibility, and target selection remain inside this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lumvise_plugin_package::{HostCompatibility, PublisherTrustStore};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;

use crate::{PluginRepository, PluginRepositoryError};

/// Environment override for durable Plugin Runtime state.
pub const LUMVISE_PLUGIN_ROOT_ENV: &str = "LUMVISE_PLUGIN_ROOT";

/// Paths and host identity required to open a Plugin Package repository.
#[derive(Clone, Debug)]
pub struct PluginRepositoryConfig {
    state_root: PathBuf,
    repository_root: PathBuf,
    publisher_trust_path: PathBuf,
    host: HostCompatibility,
}

impl PluginRepositoryConfig {
    /// Creates explicit repository configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use lumvise_plugin_package::HostCompatibility;
    /// use lumvise_plugin_runtime::PluginRepositoryConfig;
    ///
    /// let config = PluginRepositoryConfig::new(
    ///     "/var/lib/lumvise/repository",
    ///     "/var/lib/lumvise/publisher-trust.json",
    ///     HostCompatibility::new(1, "x86_64-unknown-linux-gnu"),
    /// );
    /// assert!(config.repository_root().ends_with("repository"));
    /// ```
    pub fn new(
        repository_root: impl Into<PathBuf>,
        publisher_trust_path: impl Into<PathBuf>,
        host: HostCompatibility,
    ) -> Self {
        let repository_root = repository_root.into();
        let state_root = repository_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            state_root,
            repository_root,
            publisher_trust_path: publisher_trust_path.into(),
            host,
        }
    }

    /// Resolves the canonical per-user Plugin Runtime paths and current target.
    pub fn configured() -> Self {
        let state_root = configured_state_root();
        Self {
            repository_root: state_root.join("repository"),
            publisher_trust_path: state_root.join("publisher-trust.json"),
            state_root,
            host: HostCompatibility::new(
                u32::from(CURRENT_PROTOCOL_VERSION.major),
                current_target_triple(),
            ),
        }
    }

    /// Opens the verified durable Plugin Package repository.
    pub fn open(&self) -> Result<PluginRepository, PluginRepositoryError> {
        let trust = Arc::new(PublisherTrustStore::load_json_file(
            &self.publisher_trust_path,
        )?);
        PluginRepository::open(self.repository_root.clone(), trust, self.host.clone())
    }

    /// Returns the common Plugin Runtime state root.
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Returns the durable repository root.
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    /// Returns the Publisher Trust document path.
    pub fn publisher_trust_path(&self) -> &Path {
        &self.publisher_trust_path
    }
}

fn configured_state_root() -> PathBuf {
    std::env::var_os(LUMVISE_PLUGIN_ROOT_ENV)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_plugin_root)
}

fn default_plugin_root() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumvise")
        .join("plugins")
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn current_target_triple() -> &'static str {
    "aarch64-apple-darwin"
}

#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
fn current_target_triple() -> &'static str {
    "x86_64-apple-darwin"
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
fn current_target_triple() -> &'static str {
    "aarch64-unknown-linux-gnu"
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn current_target_triple() -> &'static str {
    "x86_64-unknown-linux-gnu"
}

#[cfg(all(target_arch = "aarch64", target_os = "windows"))]
fn current_target_triple() -> &'static str {
    "aarch64-pc-windows-msvc"
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn current_target_triple() -> &'static str {
    "x86_64-pc-windows-msvc"
}

#[cfg(not(any(
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux"),
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "windows"),
    all(target_arch = "x86_64", target_os = "windows")
)))]
fn current_target_triple() -> &'static str {
    "unsupported-unknown-target"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_paths_share_one_state_root() {
        let config = PluginRepositoryConfig::new(
            "/tmp/lumvise/plugins/repository",
            "/tmp/lumvise/plugins/publisher-trust.json",
            HostCompatibility::new(1, "test-target"),
        );

        assert_eq!(config.state_root(), Path::new("/tmp/lumvise/plugins"));
        assert_eq!(
            config.repository_root(),
            Path::new("/tmp/lumvise/plugins/repository")
        );
        assert_eq!(
            config.publisher_trust_path(),
            Path::new("/tmp/lumvise/plugins/publisher-trust.json")
        );
    }

    #[test]
    fn configured_paths_share_the_runtime_state_root() {
        let config = PluginRepositoryConfig::configured();

        assert_eq!(
            config.repository_root(),
            config.state_root().join("repository")
        );
        assert_eq!(
            config.publisher_trust_path(),
            config.state_root().join("publisher-trust.json")
        );
    }

    #[test]
    fn open_creates_an_empty_verified_repository() {
        let root = tempfile::tempdir().expect("plugin runtime state root");
        let repository_root = root.path().join("repository");
        let publisher_trust_path = root.path().join("publisher-trust.json");
        std::fs::write(&publisher_trust_path, br#"{"schema_version":1,"keys":[]}"#)
            .expect("publisher trust document");
        let config = PluginRepositoryConfig::new(
            &repository_root,
            publisher_trust_path,
            HostCompatibility::new(1, "test-target"),
        );

        let repository = config.open().expect("verified repository");

        assert!(repository_root.is_dir());
        assert!(repository.registry().records.is_empty());
    }
}
