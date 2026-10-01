//! Offline administration for source-independent compiled plugin releases.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use lumvise_plugin_package::{
    PluginReleaseArtifactV2, PluginReleaseIndexError, PluginReleaseIndexV2,
};
use lumvise_plugin_runtime::{
    DenyAllHostCapabilityBroker, PluginRegistryRecord, PluginRepository, PluginRepositoryConfig,
    PluginRepositoryError, PluginRuntimeConfig, PluginSandboxError, PluginSystem, UninstallPolicy,
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
    install_artifact_with_activation(repository, release_dir, artifact, |repository, record| {
        repository.activate(runtime, &record.plugin_id, &record.version)
    })
}

fn install_artifact_with_activation<F>(
    repository: &mut PluginRepository,
    release_dir: &Path,
    artifact: &PluginReleaseArtifactV2,
    activate: F,
) -> Result<InstalledReleasePlugin, PluginAdminError>
where
    F: FnMut(&mut PluginRepository, &PluginRegistryRecord) -> Result<(), PluginRepositoryError>,
{
    let archive = release_archive_path(release_dir, Path::new(&artifact.archive_path))?;
    let (record, newly_installed) = install_or_reuse(repository, &archive)?;
    validate_artifact_identity(repository, artifact, &record, newly_installed)?;
    activate_release_record(repository, &record, newly_installed, activate)?;
    Ok(InstalledReleasePlugin {
        plugin_id: record.plugin_id,
        version: record.version,
    })
}

fn install_or_reuse(
    repository: &mut PluginRepository,
    archive: &Path,
) -> Result<(PluginRegistryRecord, bool), PluginAdminError> {
    match repository.install_archive(archive, false) {
        Ok(record) => Ok((record, true)),
        Err(PluginRepositoryError::DuplicateVersion {
            plugin_id,
            version,
            digest,
        }) => Ok((
            find_reusable_record(repository, &plugin_id, &version, &digest)?,
            false,
        )),
        Err(error) => Err(error.into()),
    }
}

fn find_reusable_record(
    repository: &PluginRepository,
    plugin_id: &str,
    version: &str,
    digest: &str,
) -> Result<PluginRegistryRecord, PluginRepositoryError> {
    repository
        .registry()
        .records
        .iter()
        .find(|record| {
            record.plugin_id == plugin_id && record.version == version && record.digest == digest
        })
        .cloned()
        .ok_or_else(|| PluginRepositoryError::DuplicateVersion {
            plugin_id: plugin_id.into(),
            version: version.into(),
            digest: digest.into(),
        })
}

fn validate_artifact_identity(
    repository: &mut PluginRepository,
    artifact: &PluginReleaseArtifactV2,
    record: &PluginRegistryRecord,
    newly_installed: bool,
) -> Result<(), PluginAdminError> {
    if record.plugin_id == artifact.plugin_id && record.version == artifact.plugin_version {
        return Ok(());
    }
    cleanup_new_record(repository, record, newly_installed)?;
    Err(invalid(
        format!("{}@{}", record.plugin_id, record.version),
        format!(
            "{}@{} from release index",
            artifact.plugin_id, artifact.plugin_version
        ),
    ))
}

fn cleanup_new_record(
    repository: &mut PluginRepository,
    record: &PluginRegistryRecord,
    newly_installed: bool,
) -> Result<(), PluginAdminError> {
    if newly_installed {
        repository.uninstall(&record.plugin_id, &record.version, UninstallPolicy::Purge)?;
    }
    Ok(())
}

fn activate_release_record<F>(
    repository: &mut PluginRepository,
    record: &PluginRegistryRecord,
    newly_installed: bool,
    mut activate: F,
) -> Result<(), PluginAdminError>
where
    F: FnMut(&mut PluginRepository, &PluginRegistryRecord) -> Result<(), PluginRepositoryError>,
{
    if let Err(error) = activate(repository, record) {
        let _ = cleanup_new_record(repository, record, newly_installed);
        return Err(error.into());
    }
    Ok(())
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
    use ed25519_dalek::SigningKey;
    use lumvise_plugin_package::{
        BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility,
        PluginManifest, ProtocolRange, PublisherIdentity, PublisherTrustStore, build_package,
    };
    use lumvise_plugin_runtime::{PluginRegistryRecord, PluginRepositoryError};
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, fs};

    const TEST_TARGET: &str = "admin-test-host";
    const EXECUTABLE_PATH: &str = "bin/plugin";

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

    #[test]
    fn exact_signed_archive_can_be_reused() {
        let root = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[19; 32]);
        let (release_dir, artifact) = signed_release(&root, &key, "same-content");
        let mut repository = test_repository(root.path(), &key);
        let mut activations = 0;
        let mut activate = |_: &mut PluginRepository, _: &PluginRegistryRecord| {
            activations += 1;
            Ok(())
        };

        let first = install_artifact_with_activation(
            &mut repository,
            &release_dir,
            &artifact,
            &mut activate,
        )
        .unwrap();
        let repeated = install_artifact_with_activation(
            &mut repository,
            &release_dir,
            &artifact,
            &mut activate,
        )
        .unwrap();

        assert_eq!(first, repeated);
        assert_eq!(activations, 2);
        assert_eq!(repository.registry().records.len(), 1);
    }

    #[test]
    fn changed_signed_content_at_same_version_keeps_collision_error() {
        let root = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[20; 32]);
        let (release_dir, first_artifact) = signed_release(&root, &key, "first-content");
        let (changed_release_dir, changed_artifact) =
            signed_release(&root, &key, "changed-content");
        let mut repository = test_repository(root.path(), &key);

        install_artifact_with_activation(&mut repository, &release_dir, &first_artifact, |_, _| {
            Ok(())
        })
        .unwrap();
        let error = install_artifact_with_activation(
            &mut repository,
            &changed_release_dir,
            &changed_artifact,
            |_, _| Ok(()),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PluginAdminError::Repository(PluginRepositoryError::VersionCollision { .. })
        ));
        assert_eq!(repository.registry().records.len(), 1);
    }

    #[test]
    fn failed_activation_does_not_remove_reused_record() {
        let root = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[21; 32]);
        let (release_dir, artifact) = signed_release(&root, &key, "retained-content");
        let mut repository = test_repository(root.path(), &key);
        install_artifact_with_activation(&mut repository, &release_dir, &artifact, |_, _| Ok(()))
            .unwrap();

        let error =
            install_artifact_with_activation(&mut repository, &release_dir, &artifact, |_, _| {
                Err(PluginRepositoryError::InvalidRegistry {
                    path: root.path().to_path_buf(),
                    message: "test activation failure".into(),
                })
            })
            .unwrap_err();

        assert!(error.to_string().contains("test activation failure"));
        assert_eq!(repository.registry().records.len(), 1);
    }

    #[test]
    fn identity_mismatch_removes_only_the_new_registration() {
        let root = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[22; 32]);
        let (release_dir, mut artifact) = signed_release(&root, &key, "identity-content");
        let mut repository = test_repository(root.path(), &key);
        install_artifact_with_activation(&mut repository, &release_dir, &artifact, |_, _| Ok(()))
            .unwrap();
        artifact.plugin_id = "admin.other".into();

        let error =
            install_artifact_with_activation(&mut repository, &release_dir, &artifact, |_, _| {
                Ok(())
            })
            .unwrap_err();

        assert!(matches!(error, PluginAdminError::InvalidValue { .. }));
        assert_eq!(repository.registry().records.len(), 1);

        let mut new_repository = test_repository(&root.path().join("new"), &key);
        let error = install_artifact_with_activation(
            &mut new_repository,
            &release_dir,
            &artifact,
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, PluginAdminError::InvalidValue { .. }));
        assert!(new_repository.registry().records.is_empty());
    }

    #[test]
    fn failed_activation_removes_a_new_registration() {
        let root = tempfile::TempDir::new().unwrap();
        let key = SigningKey::from_bytes(&[23; 32]);
        let (release_dir, artifact) = signed_release(&root, &key, "new-failure-content");
        let mut repository = test_repository(root.path(), &key);

        let error =
            install_artifact_with_activation(&mut repository, &release_dir, &artifact, |_, _| {
                Err(PluginRepositoryError::InvalidRegistry {
                    path: root.path().to_path_buf(),
                    message: "test activation failure".into(),
                })
            })
            .unwrap_err();

        assert!(error.to_string().contains("test activation failure"));
        assert!(repository.registry().records.is_empty());
    }

    fn test_repository(root: &Path, key: &SigningKey) -> PluginRepository {
        let mut trust = PublisherTrustStore::new();
        trust
            .add_key("admin.test", "admin.test.key", key.verifying_key())
            .unwrap();
        PluginRepository::open(
            root.join("repository"),
            Arc::new(trust),
            HostCompatibility::new(1, TEST_TARGET),
        )
        .unwrap()
    }

    fn signed_release(
        root: &tempfile::TempDir,
        key: &SigningKey,
        payload: &str,
    ) -> (PathBuf, PluginReleaseArtifactV2) {
        let release_dir = root.path().join(payload);
        fs::create_dir_all(&release_dir).unwrap();
        let archive = release_dir.join("plugin.lvp");
        build_test_package(&archive, key, payload.as_bytes());
        let bytes = fs::read(&archive).unwrap();
        let sha256 = hex::encode(Sha256::digest(bytes));
        (
            release_dir,
            test_artifact("admin.fixture", "1.0.0", &sha256),
        )
    }

    fn build_test_package(archive: &Path, key: &SigningKey, payload: &[u8]) {
        let manifest = PluginManifest {
            schema_version: 1,
            publisher: PublisherIdentity {
                publisher_id: "admin.test".into(),
                key_id: "admin.test.key".into(),
            },
            plugin_id: "admin.fixture".into(),
            plugin_version: "1.0.0".into(),
            protocol: ProtocolRange { min: 1, max: 1 },
            targets: BTreeMap::from([(TEST_TARGET.into(), EXECUTABLE_PATH.into())]),
            files: BTreeMap::from([(EXECUTABLE_PATH.into(), hex::encode(Sha256::digest(payload)))]),
            exports: vec![ExportDescriptor {
                id: "fixture.run".into(),
                name: "Fixture".into(),
                description: String::new(),
                surface: ExportSurface::McpTool,
                input_schema: serde_json::json!({"type":"object"}),
                output_schema: serde_json::json!({"type":"object"}),
                admission: None,
                execution: ExecutionMode::Foreground,
            }],
            host_capabilities: Vec::new(),
        };
        build_package(
            archive,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([(EXECUTABLE_PATH.into(), payload.to_vec())]),
                signing_key: key,
            },
        )
        .unwrap();
    }

    fn test_artifact(plugin_id: &str, version: &str, digest: &str) -> PluginReleaseArtifactV2 {
        PluginReleaseArtifactV2 {
            plugin_id: plugin_id.into(),
            plugin_version: version.into(),
            publisher_id: "admin.test".into(),
            key_id: "admin.test.key".into(),
            archive_path: "plugin.lvp".into(),
            archive_sha256: digest.into(),
            targets: vec![TEST_TARGET.into()],
        }
    }
}
