use std::{collections::BTreeMap, fs, path::Path};

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use crate::{BuildPackageRequest, PackageError, PluginManifest, build_package};

/// Builds a deterministic package from an exact source-free payload directory.
///
/// `manifest.files` supplies the expected canonical package paths. Existing
/// digest values are replaced with SHA-256 digests computed from the payload.
/// Extra, missing, and symlinked payloads are rejected before publication.
///
/// # Example
/// ```no_run
/// # use std::path::Path;
/// # use ed25519_dalek::SigningKey;
/// # use lumvise_plugin_package::PluginManifest;
/// # fn package(manifest: PluginManifest) -> Result<(), lumvise_plugin_package::PackageError> {
/// let key = SigningKey::from_bytes(&[7; 32]);
/// lumvise_plugin_package::build_package_from_directory(
///     Path::new("release/plugin.lvp"),
///     manifest,
///     Path::new("release/payload"),
///     &key,
/// )?;
/// # Ok(())
/// # }
/// ```
pub fn build_package_from_directory(
    output_path: &Path,
    mut manifest: PluginManifest,
    payload_root: &Path,
    signing_key: &SigningKey,
) -> Result<(), PackageError> {
    let files = read_payload_tree(payload_root)?;
    require_exact_paths(&manifest.files, &files)?;
    manifest.files = payload_hashes(&files);
    build_package(
        output_path,
        BuildPackageRequest {
            manifest,
            files,
            signing_key,
        },
    )
}

fn read_payload_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, PackageError> {
    reject_symlink(root, "payload root")?;
    let mut files = BTreeMap::new();
    read_directory(root, root, &mut files)?;
    Ok(files)
}

fn read_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(directory, source))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        read_entry(root, &entry.path(), files)?;
    }
    Ok(())
}

fn read_entry(
    root: &Path,
    path: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    let package_path = package_path(root, path)?;
    reject_symlink(path, &package_path)?;
    let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.is_dir() {
        return read_directory(root, path, files);
    }
    if !metadata.is_file() {
        return Err(PackageError::UnsafePath(package_path));
    }
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    files.insert(package_path, bytes);
    Ok(())
}

fn package_path(root: &Path, path: &Path) -> Result<String, PackageError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| PackageError::UnsafePath(path.display().to_string()))?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| PackageError::UnsafePath(relative.display().to_string()))?;
    Ok(parts.join("/"))
}

fn reject_symlink(path: &Path, package_path: &str) -> Result<(), PackageError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(PackageError::Symlink(package_path.to_owned()));
    }
    Ok(())
}

fn require_exact_paths(
    expected: &BTreeMap<String, String>,
    actual: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    if let Some(path) = actual.keys().find(|path| !expected.contains_key(*path)) {
        return Err(PackageError::UnsignedFile(path.clone()));
    }
    if let Some(path) = expected.keys().find(|path| !actual.contains_key(*path)) {
        return Err(PackageError::MissingFile(path.clone()));
    }
    Ok(())
}

fn payload_hashes(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(path, bytes)| (path.clone(), hex::encode(Sha256::digest(bytes))))
        .collect()
}

fn io_error(path: &Path, source: std::io::Error) -> PackageError {
    PackageError::Io {
        path: path.to_path_buf(),
        source,
    }
}
