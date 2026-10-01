use std::collections::BTreeMap;
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{BuiltinReleaseDescriptor, BuiltinReleaseError, BuiltinReleaseMatrix, Result};
use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    HostCompatibility, PluginManifest, PluginReleaseArtifactV2, PluginReleaseIndexV2,
    ReleaseComposition, build_package_from_directory, install_verified_package,
    remove_installed_package, verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct PublisherTrustDocument {
    schema_version: u32,
    keys: Vec<PublisherTrustKey>,
}

#[derive(Serialize)]
struct PublisherTrustKey {
    publisher_id: String,
    key_id: String,
    public_key_hex: String,
    revoked: bool,
}

#[derive(Serialize)]
struct HostCapabilityGrantDocument {
    schema_version: u32,
    grants: Vec<HostCapabilityGrant>,
}

#[derive(Serialize)]
struct HostCapabilityGrant {
    plugin_id: String,
    capability_id: String,
}

/// Publishes one explicit full or minimal schema-two composition.
pub fn publish_builtin_release_composition(
    matrix: &BuiltinReleaseMatrix,
    signing_key: &SigningKey,
    output_dir: &Path,
    composition: ReleaseComposition,
) -> Result<PluginReleaseIndexV2> {
    matrix.validate()?;
    let plugin_ids = selected_plugin_ids(matrix, composition)?;
    let parent = prepare_destination(output_dir)?;
    let release = temporary_directory(&parent, ".builtin-release-")?;
    let validation = temporary_directory(&parent, ".builtin-install-")?;
    let artifacts = publish_artifacts(
        matrix,
        &plugin_ids,
        signing_key,
        release.path(),
        validation.path(),
    )?;
    let index = PluginReleaseIndexV2 {
        schema_version: lumvise_plugin_package::PLUGIN_RELEASE_INDEX_SCHEMA_VERSION,
        product: "lumvise".into(),
        composition,
        artifacts,
    };
    write_release_policies(matrix, signing_key, &index.artifacts, release.path())?;
    write_json_file(&release.path().join("builtins-release.json"), &index)?;
    publish_directory(release, output_dir)?;
    Ok(index)
}

fn selected_plugin_ids(
    matrix: &BuiltinReleaseMatrix,
    composition: ReleaseComposition,
) -> Result<Vec<String>> {
    let plugin_ids = matrix
        .descriptors
        .keys()
        .filter(|plugin_id| {
            composition == ReleaseComposition::Full
                || matches!(plugin_id.as_str(), "builtin.semantic" | "builtin.knowledge")
        })
        .cloned()
        .collect::<Vec<_>>();
    if composition == ReleaseComposition::Minimal
        && plugin_ids != ["builtin.knowledge", "builtin.semantic"]
    {
        return Err(BuiltinReleaseError::InvalidInput {
            value: plugin_ids.join(","),
            expected: "minimal descriptors builtin.knowledge,builtin.semantic".into(),
        });
    }
    Ok(plugin_ids)
}
fn write_release_policies(
    matrix: &BuiltinReleaseMatrix,
    signing_key: &SigningKey,
    artifacts: &[PluginReleaseArtifactV2],
    output_dir: &Path,
) -> Result<()> {
    let public_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
    let mut publisher_keys = std::collections::BTreeSet::new();
    let mut grants = std::collections::BTreeSet::new();
    for artifact in artifacts {
        publisher_keys.insert((artifact.publisher_id.clone(), artifact.key_id.clone()));
        for capability in &matrix.manifests[&artifact.plugin_id].host_capabilities {
            grants.insert((artifact.plugin_id.clone(), capability.id.clone()));
        }
    }
    let trust = PublisherTrustDocument {
        schema_version: 1,
        keys: publisher_keys
            .into_iter()
            .map(|(publisher_id, key_id)| PublisherTrustKey {
                publisher_id,
                key_id,
                public_key_hex: public_key_hex.clone(),
                revoked: false,
            })
            .collect(),
    };
    let grants = HostCapabilityGrantDocument {
        schema_version: 1,
        grants: grants
            .into_iter()
            .map(|(plugin_id, capability_id)| HostCapabilityGrant {
                plugin_id,
                capability_id,
            })
            .collect(),
    };
    write_json_file(&output_dir.join("publisher-trust.json"), &trust)?;
    write_json_file(&output_dir.join("host-capability-grants.json"), &grants)
}

fn write_json_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|source| io_error(path, source))
}

fn publish_artifacts(
    matrix: &BuiltinReleaseMatrix,
    plugin_ids: &[String],
    signing_key: &SigningKey,
    output_dir: &Path,
    validation_root: &Path,
) -> Result<Vec<PluginReleaseArtifactV2>> {
    plugin_ids
        .iter()
        .map(|plugin_id| publish_one(plugin_id, matrix, signing_key, output_dir, validation_root))
        .collect()
}

fn publish_one(
    plugin_id: &str,
    matrix: &BuiltinReleaseMatrix,
    key: &SigningKey,
    output_dir: &Path,
    validation_root: &Path,
) -> Result<PluginReleaseArtifactV2> {
    let manifest = merged_manifest(plugin_id, matrix)?;
    let archive_path = output_dir.join(format!(
        "{}-{}.lvp",
        manifest.plugin_id, manifest.plugin_version
    ));
    let staging = tempfile::Builder::new()
        .prefix(".payload-")
        .tempdir_in(output_dir)
        .map_err(|source| io_error(output_dir, source))?;
    stage_payload(plugin_id, matrix, &manifest, staging.path())?;
    build_package_from_directory(&archive_path, manifest.clone(), staging.path(), key)?;
    verify_and_install(&archive_path, matrix, key, validation_root)?;
    artifact_metadata(manifest, archive_path)
}

fn merged_manifest(plugin_id: &str, matrix: &BuiltinReleaseMatrix) -> Result<PluginManifest> {
    let descriptor = &matrix.descriptors[plugin_id];
    let mut manifest = matrix.manifests[plugin_id].clone();
    for (target, binaries) in &matrix.targets {
        let binary_path = &binaries[plugin_id];
        let bytes = fs::read(binary_path).map_err(|source| io_error(binary_path, source))?;
        let package_path = descriptor.payload_pattern.replace("{target}", target);
        manifest
            .targets
            .insert(target.clone(), package_path.clone());
        manifest.files.insert(package_path, hex_digest(&bytes));
    }
    // Digests are recomputed by `build_package_from_directory`; the manifest
    // only needs the canonical asset paths.
    for package_path in payload_assets(descriptor)?.keys() {
        manifest.files.insert(package_path.clone(), String::new());
    }
    Ok(manifest)
}

/// Collects the descriptor's payload assets as package-relative paths to
/// on-disk source files. `payload_assets_dir` is crate-relative and doubles
/// as the package-relative prefix for every file inside it.
fn payload_assets(descriptor: &BuiltinReleaseDescriptor) -> Result<BTreeMap<String, PathBuf>> {
    let Some(assets_root) = &descriptor.payload_assets_path else {
        return Ok(BTreeMap::new());
    };
    let prefix = descriptor.payload_assets_dir.as_deref().unwrap_or_default();
    let mut assets = BTreeMap::new();
    collect_assets(prefix, assets_root, &mut assets)?;
    Ok(assets)
}

fn collect_assets(
    prefix: &str,
    directory: &Path,
    assets: &mut BTreeMap<String, PathBuf>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| io_error(directory, error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| io_error(directory, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let package_path = format!("{prefix}/{name}");
        if path.is_dir() {
            collect_assets(&package_path, &path, assets)?;
        } else {
            assets.insert(package_path, path);
        }
    }
    Ok(())
}

fn stage_payload(
    plugin_id: &str,
    matrix: &BuiltinReleaseMatrix,
    manifest: &PluginManifest,
    staging: &Path,
) -> Result<()> {
    for (target, binaries) in &matrix.targets {
        let package_path = &manifest.targets[target];
        copy_payload(&binaries[plugin_id], staging, package_path)?;
    }
    for (package_path, source) in &payload_assets(&matrix.descriptors[plugin_id])? {
        copy_payload(source, staging, package_path)?;
    }
    Ok(())
}

fn copy_payload(source: &Path, staging: &Path, package_path: &str) -> Result<()> {
    let bytes = fs::read(source).map_err(|error| io_error(source, error))?;
    write_payload(&bytes, staging, package_path)
}

fn write_payload(bytes: &[u8], staging: &Path, package_path: &str) -> Result<()> {
    let destination = staging.join(package_path);
    let parent = destination
        .parent()
        .ok_or_else(|| BuiltinReleaseError::InvalidInput {
            value: package_path.into(),
            expected: "package payload path with a parent directory".into(),
        })?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    fs::write(&destination, bytes).map_err(|error| io_error(&destination, error))
}

fn verify_and_install(
    archive_path: &Path,
    matrix: &BuiltinReleaseMatrix,
    key: &SigningKey,
    validation_root: &Path,
) -> Result<()> {
    let mut installed = false;
    for target in matrix.targets.keys() {
        let host = HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), target);
        let verified = verify_package(archive_path, &key.verifying_key(), &host)?;
        if !installed {
            let installed_package = install_verified_package(&verified, validation_root)?;
            remove_installed_package(&installed_package)?;
            installed = true;
        }
    }
    Ok(())
}

fn artifact_metadata(
    manifest: PluginManifest,
    archive_path: PathBuf,
) -> Result<PluginReleaseArtifactV2> {
    let bytes = fs::read(&archive_path).map_err(|source| io_error(&archive_path, source))?;
    let published_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BuiltinReleaseError::InvalidInput {
            value: archive_path.display().to_string(),
            expected: "UTF-8 published archive filename".into(),
        })?;
    Ok(PluginReleaseArtifactV2 {
        plugin_id: manifest.plugin_id,
        plugin_version: manifest.plugin_version,
        publisher_id: manifest.publisher.publisher_id,
        key_id: manifest.publisher.key_id,
        archive_sha256: hex_digest(&bytes),
        archive_path: published_name.into(),
        targets: manifest.targets.into_keys().collect(),
    })
}

fn prepare_destination(output_dir: &Path) -> Result<PathBuf> {
    let parent = output_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    if output_dir.exists()
        && fs::read_dir(output_dir)
            .map_err(|error| io_error(output_dir, error))?
            .next()
            .is_some()
    {
        return Err(BuiltinReleaseError::InvalidInput {
            value: output_dir.display().to_string(),
            expected: "missing or empty built-in release destination".into(),
        });
    }
    Ok(parent.to_path_buf())
}

fn temporary_directory(parent: &Path, prefix: &str) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .map_err(|error| io_error(parent, error))
}

fn publish_directory(release: tempfile::TempDir, output_dir: &Path) -> Result<()> {
    if output_dir.exists() {
        fs::remove_dir(output_dir).map_err(|error| io_error(output_dir, error))?;
    }
    let release_path = release.keep();
    fs::rename(&release_path, output_dir).map_err(|error| io_error(output_dir, error))
}

fn hex_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn io_error(path: &Path, source: std::io::Error) -> BuiltinReleaseError {
    BuiltinReleaseError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinBinaryPaths, BuiltinReleaseDescriptor};
    use lumvise_plugin_package::{ProtocolRange, PublisherIdentity};
    use std::collections::BTreeMap;

    #[test]
    fn assistant_staging_contains_no_placeholder_views() {
        let workspace = tempfile::tempdir().expect("release workspace");
        let binary = workspace.path().join("assistant");
        fs::write(&binary, b"assistant").expect("fixture binary");
        let matrix = matrix_with_assets(binary, None);
        let manifest = merged_manifest("builtin.assistant", &matrix).expect("manifest");
        let staging = tempfile::tempdir().expect("payload staging");
        stage_payload("builtin.assistant", &matrix, &manifest, staging.path())
            .expect("stage payload");

        assert!(!staging.path().join("views").exists());
        assert_eq!(manifest.files.len(), 1);
    }

    #[test]
    fn view_assets_are_staged_and_listed_in_the_merged_manifest() {
        let workspace = tempfile::tempdir().expect("release workspace");
        let assets_root = workspace.path().join("crate/views/canvas");
        fs::create_dir_all(assets_root.join("nested")).expect("assets tree");
        fs::write(assets_root.join("index.html"), b"<html>").expect("asset");
        fs::write(assets_root.join("nested/app.js"), b"export 1;").expect("asset");
        let binary = workspace.path().join("assistant");
        fs::write(&binary, b"assistant").expect("fixture binary");
        let matrix = matrix_with_assets(binary, Some(("views/canvas".into(), assets_root.clone())));

        let manifest = merged_manifest("builtin.assistant", &matrix).expect("manifest");
        assert!(
            manifest
                .files
                .contains_key("plugins/builtin.assistant/aarch64-apple-darwin/assistant")
        );
        assert!(manifest.files.contains_key("views/canvas/index.html"));
        assert!(manifest.files.contains_key("views/canvas/nested/app.js"));

        let staging = tempfile::tempdir().expect("payload staging");
        stage_payload("builtin.assistant", &matrix, &manifest, staging.path())
            .expect("stage payload");
        assert_eq!(
            fs::read(staging.path().join("views/canvas/index.html")).expect("staged asset"),
            b"<html>"
        );
        assert_eq!(
            fs::read(staging.path().join("views/canvas/nested/app.js")).expect("staged asset"),
            b"export 1;"
        );

        // The staged payload must exactly match the manifest file map: the
        // builder rejects extra or missing payloads and recomputes digests.
        let archive = workspace.path().join("builtin.assistant-1.0.0.lvp");
        build_package_from_directory(
            &archive,
            manifest,
            staging.path(),
            &SigningKey::from_bytes(&[7; 32]),
        )
        .expect("build package");
    }

    fn matrix_with_assets(
        binary: PathBuf,
        assets: Option<(String, PathBuf)>,
    ) -> BuiltinReleaseMatrix {
        let (payload_assets_dir, payload_assets_path) = match assets {
            Some((dir, path)) => (Some(dir), Some(path)),
            None => (None, None),
        };
        let descriptor = BuiltinReleaseDescriptor {
            package: "lumvise-plugin-assistant".into(),
            binary: "lumvise-plugin-assistant".into(),
            plugin_id: "builtin.assistant".into(),
            plugin_version: "1.0.0".into(),
            publisher_id: "lumvise.builtins".into(),
            key_id: "lumvise-builtin-release".into(),
            protocol_min: 1,
            protocol_max: 1,
            payload_pattern: "plugins/builtin.assistant/{target}/assistant".into(),
            manifest_template: "lumvise-plugin-manifest.json".into(),
            payload_assets_dir,
            payload_assets_path,
            crate_path: PathBuf::new(),
            workspace_root: PathBuf::new(),
        };
        let manifest = PluginManifest {
            schema_version: 1,
            publisher: PublisherIdentity {
                publisher_id: descriptor.publisher_id.clone(),
                key_id: descriptor.key_id.clone(),
            },
            plugin_id: descriptor.plugin_id.clone(),
            plugin_version: descriptor.plugin_version.clone(),
            protocol: ProtocolRange { min: 1, max: 1 },
            targets: BTreeMap::new(),
            files: BTreeMap::new(),
            exports: Vec::new(),
            host_capabilities: Vec::new(),
        };
        BuiltinReleaseMatrix {
            descriptors: BTreeMap::from([(descriptor.plugin_id.clone(), descriptor)]),
            manifests: BTreeMap::from([("builtin.assistant".into(), manifest)]),
            targets: BTreeMap::from([(
                "aarch64-apple-darwin".into(),
                BuiltinBinaryPaths::from([("builtin.assistant".into(), binary)]),
            )]),
        }
    }
}
