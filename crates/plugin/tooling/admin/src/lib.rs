//! Offline administration for source-independent compiled plugin releases.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use lumvise_plugin_package::{
    PluginReleaseArtifactV2, PluginReleaseIndexError, PluginReleaseIndexV2,
};
use lumvise_plugin_runtime::{
    DenyAllHostCapabilityBroker, PluginRepository, PluginRepositoryConfig, PluginRepositoryError,
    PluginRuntimeConfig, PluginSandboxError, PluginSystem, UninstallPolicy,
};

/// Plugin administration failure with the offending release value or path.
#[derive(Debug, thiserror::Error)]
pub enum PluginAdminError {
    /// A release file could not be read.
    #[error("plugin release I/O failed for `{path}`: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Release JSON did not match the shared product index contract.
    #[error(transparent)]
    Index(#[from] PluginReleaseIndexError),
    /// Release metadata referenced an invalid archive.
    #[error("invalid plugin release value `{value}`; expected {expected}")]
    InvalidValue { value: String, expected: String },
    /// Plugin Runtime rejected repository or lifecycle work.
    #[error(transparent)]
    Repository(#[from] PluginRepositoryError),
    /// Plugin sandbox startup failed.
    #[error(transparent)]
    Sandbox(#[from] PluginSandboxError),
}

/// Installed and enabled plugin identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledReleasePlugin {
    /// Signed plugin identity.
    pub plugin_id: String,
    /// Signed plugin version.
    pub version: String,
}

/// Verifies, installs, and enables every archive in one built-in release.
///
/// # Example
///
/// ```ignore
/// let installed = lumvise_plugin_admin::install_release("dist/builtins")?;
/// assert_eq!(installed.len(), 4);
/// ```
pub fn install_release(
    release_dir: impl AsRef<Path>,
) -> Result<Vec<InstalledReleasePlugin>, PluginAdminError> {
    let release_dir = release_dir.as_ref();
    let index = PluginReleaseIndexV2::read(release_dir.join("builtins-release.json"))?;
    index.verify_artifacts(release_dir)?;
    let mut repository = PluginRepositoryConfig::configured().open()?;
    let runtime = PluginSystem::production(
        PluginRuntimeConfig::default(),
        Arc::new(DenyAllHostCapabilityBroker),
    )?;
    repository.restore_into(&runtime)?;
    index
        .artifacts
        .iter()
        .map(|artifact| install_artifact(&mut repository, &runtime, release_dir, artifact))
        .collect()
}

fn install_artifact(
    repository: &mut PluginRepository,
    runtime: &PluginSystem,
    release_dir: &Path,
    artifact: &PluginReleaseArtifactV2,
) -> Result<InstalledReleasePlugin, PluginAdminError> {
    let archive = release_archive_path(release_dir, Path::new(&artifact.archive_path))?;
    let record = repository.install_archive(&archive, false)?;
    if record.plugin_id != artifact.plugin_id || record.version != artifact.plugin_version {
        repository.uninstall(&record.plugin_id, &record.version, UninstallPolicy::Purge)?;
        return Err(invalid(
            format!("{}@{}", record.plugin_id, record.version),
            format!(
                "{}@{} from release index",
                artifact.plugin_id, artifact.plugin_version
            ),
        ));
    }
    if let Err(error) = repository.activate(runtime, &record.plugin_id, &record.version) {
        let _ = repository.uninstall(&record.plugin_id, &record.version, UninstallPolicy::Purge);
        return Err(error.into());
    }
    Ok(InstalledReleasePlugin {
        plugin_id: record.plugin_id,
        version: record.version,
    })
}

fn release_archive_path(
    release_dir: &Path,
    listed_path: &Path,
) -> Result<PathBuf, PluginAdminError> {
    let filename = listed_path.file_name().ok_or_else(|| {
        invalid(
            listed_path.display().to_string(),
            "archive filename inside release directory",
        )
    })?;
    let archive = release_dir.join(filename);
    if !archive.is_file() {
        return Err(invalid(
            archive.display().to_string(),
            "existing .lvp archive in release directory",
        ));
    }
    Ok(archive)
}

fn invalid(value: impl Into<String>, expected: impl Into<String>) -> PluginAdminError {
    PluginAdminError::InvalidValue {
        value: value.into(),
        expected: expected.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn release_index_rejects_unknown_schema() {
        let error = PluginReleaseIndexV2::parse(
            br#"{"schema_version":1,"product":"lumvise","composition":"full","artifacts":[]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("schema version"));
    }

    #[test]
    fn release_archive_stays_inside_release_directory() {
        let root = tempfile::TempDir::new().unwrap();
        fs::write(root.path().join("plugin.lvp"), b"archive").unwrap();
        let archive = release_archive_path(root.path(), Path::new("../../plugin.lvp")).unwrap();
        assert_eq!(archive, root.path().join("plugin.lvp"));
    }
}
