use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Seek, Write},
    path::Path,
};

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

use crate::{PackageError, PluginManifest, archive::validate_path};

const MANIFEST_PATH: &str = "manifest.json";
const SIGNATURE_PATH: &str = "signature.ed25519";
const SIGNATURE_DOMAIN: &[u8] = b"LUMVISE_PLUGIN_PACKAGE_V1\0";

/// Inputs for one deterministic signed `.lvp` build.
///
/// The signing key is borrowed only for the duration of [`build_package`] and
/// is never retained in the returned package or an error value.
pub struct BuildPackageRequest<'key> {
    /// Canonical manifest source containing the expected SHA-256 file tree.
    pub manifest: PluginManifest,
    /// Canonical package paths and their exact payload bytes.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Offline package signing key.
    pub signing_key: &'key SigningKey,
}

/// Writes a deterministic, signed `.lvp` ZIP package.
///
/// Entries are emitted in lexical order with fixed timestamps, stored
/// compression, and fixed Unix permissions. Repeating the same request with
/// the same key produces identical bytes. Publication atomically replaces an
/// existing destination only after the complete temporary package is synced.
///
/// # Errors
/// Returns [`PackageError`] for unsafe or reserved paths, file-tree mismatch,
/// serialization, ZIP, or output I/O failures.
pub fn build_package(
    output_path: &Path,
    request: BuildPackageRequest<'_>,
) -> Result<(), PackageError> {
    validate_file_tree(&request.manifest, &request.files)?;
    let manifest = serde_json::to_vec(&request.manifest)
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
    let signature = request
        .signing_key
        .sign(&[SIGNATURE_DOMAIN, &manifest].concat());
    let executable_paths = request
        .manifest
        .targets
        .values()
        .cloned()
        .collect::<HashSet<_>>();
    let entries = package_entries(manifest, signature.to_bytes(), request.files);
    publish_archive(output_path, entries, &executable_paths)
}

fn validate_file_tree(
    manifest: &PluginManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    validate_payload_paths(manifest, files)?;
    validate_file_membership(manifest, files)?;
    validate_file_hashes(manifest, files)
}

fn validate_payload_paths(
    manifest: &PluginManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    for path in files.keys().chain(manifest.files.keys()) {
        validate_path(path)?;
        if matches!(path.as_str(), MANIFEST_PATH | SIGNATURE_PATH) {
            return Err(PackageError::ReservedPackagePath(path.clone()));
        }
    }
    Ok(())
}

fn validate_file_membership(
    manifest: &PluginManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    for path in files.keys() {
        if !manifest.files.contains_key(path) {
            return Err(PackageError::UnsignedFile(path.clone()));
        }
    }
    for path in manifest.files.keys() {
        if !files.contains_key(path) {
            return Err(PackageError::MissingFile(path.clone()));
        }
    }
    Ok(())
}

fn validate_file_hashes(
    manifest: &PluginManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageError> {
    for (path, expected) in &manifest.files {
        let bytes = files
            .get(path)
            .ok_or_else(|| PackageError::MissingFile(path.clone()))?;
        let actual = hex::encode(Sha256::digest(bytes));
        if &actual != expected {
            return Err(PackageError::HashMismatch {
                path: path.clone(),
                expected: expected.clone(),
                actual,
            });
        }
    }
    Ok(())
}

fn package_entries(
    manifest: Vec<u8>,
    signature: [u8; 64],
    mut files: BTreeMap<String, Vec<u8>>,
) -> BTreeMap<String, Vec<u8>> {
    files.insert(MANIFEST_PATH.into(), manifest);
    files.insert(SIGNATURE_PATH.into(), signature.to_vec());
    files
}

fn publish_archive(
    output_path: &Path,
    entries: BTreeMap<String, Vec<u8>>,
    executable_paths: &HashSet<String>,
) -> Result<(), PackageError> {
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".lvp-build-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(|source| PackageError::Io {
            path: output_path.to_path_buf(),
            source,
        })?;
    write_archive(temporary.as_file_mut(), entries, executable_paths)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| PackageError::Io {
            path: output_path.to_path_buf(),
            source,
        })?;
    temporary
        .persist(output_path)
        .map_err(|error| PackageError::Io {
            path: output_path.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

fn write_archive(
    file: &mut File,
    entries: BTreeMap<String, Vec<u8>>,
    executable_paths: &HashSet<String>,
) -> Result<(), PackageError> {
    let mut archive = ZipWriter::new(file);
    for (path, bytes) in entries {
        let mode = payload_mode(&path, executable_paths);
        write_entry(&mut archive, &path, &bytes, mode)?;
    }
    archive
        .finish()
        .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
    Ok(())
}

fn payload_mode(path: &str, executable_paths: &HashSet<String>) -> u32 {
    if executable_paths.contains(path) {
        0o755
    } else {
        0o644
    }
}

fn write_entry<W: Write + Seek>(
    archive: &mut ZipWriter<W>,
    path: &str,
    bytes: &[u8],
    unix_mode: u32,
) -> Result<(), PackageError> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default())
        .unix_permissions(unix_mode);
    archive
        .start_file(path, options)
        .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
    archive
        .write_all(bytes)
        .map_err(|error| PackageError::InvalidArchive(error.to_string()))
}
