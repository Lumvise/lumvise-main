use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Cursor, Read, Seek},
    path::Path,
};

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{HostCompatibility, PackageError, PluginManifest, VerificationLimits};

const MANIFEST_PATH: &str = "manifest.json";
const SIGNATURE_PATH: &str = "signature.ed25519";
const SIGNATURE_DOMAIN: &[u8] = b"LUMVISE_PLUGIN_PACKAGE_V1\0";

pub(crate) struct VerifiedPackageContents {
    pub(crate) manifest: PluginManifest,
    pub(crate) package_digest: String,
    pub(crate) executable_path: String,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
}

pub(crate) struct LoadedArchive {
    bytes: Vec<u8>,
}

impl LoadedArchive {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub(crate) fn verify(
    package_path: &Path,
    trusted_key: &VerifyingKey,
    host: &HostCompatibility,
    limits: &VerificationLimits,
) -> Result<VerifiedPackageContents, PackageError> {
    let loaded = load(package_path, limits)?;
    verify_loaded(&loaded, trusted_key, host, limits)
}

pub(crate) fn load(
    package_path: &Path,
    limits: &VerificationLimits,
) -> Result<LoadedArchive, PackageError> {
    let bytes = read_archive(package_path, limits.max_archive_bytes)?;
    validate_central_directory_names(&bytes)?;
    Ok(LoadedArchive { bytes })
}

pub(crate) fn manifest(
    loaded: &LoadedArchive,
    limits: &VerificationLimits,
) -> Result<PluginManifest, PackageError> {
    let mut archive = ZipArchive::new(Cursor::new(loaded.bytes.as_slice()))
        .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
    let mut entry = archive
        .by_name(MANIFEST_PATH)
        .map_err(|_| PackageError::MissingEntry(MANIFEST_PATH))?;
    let mut total = 0;
    validate_entry_size(MANIFEST_PATH, entry.size(), &mut total, limits)?;
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
    parse_canonical_manifest(&bytes)
}

pub(crate) fn verify_loaded(
    loaded: &LoadedArchive,
    trusted_key: &VerifyingKey,
    host: &HostCompatibility,
    limits: &VerificationLimits,
) -> Result<VerifiedPackageContents, PackageError> {
    let mut archive = ZipArchive::new(Cursor::new(loaded.bytes.as_slice()))
        .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
    let entries = read_entries(&mut archive, limits)?;
    let manifest_bytes = required_entry(&entries, MANIFEST_PATH)?;
    let manifest = parse_canonical_manifest(manifest_bytes)?;
    let package_digest = hex::encode(Sha256::digest(manifest_bytes));
    let executable_path = manifest.validate(host)?.to_owned();

    verify_signature(
        manifest_bytes,
        required_entry(&entries, SIGNATURE_PATH)?,
        trusted_key,
    )?;
    let files = verify_file_tree(&manifest, entries)?;
    if !files.contains_key(&executable_path) {
        return Err(PackageError::UnlistedExecutable(executable_path));
    }
    Ok(VerifiedPackageContents {
        manifest,
        package_digest,
        executable_path,
        files,
    })
}

fn read_entries<R: Read + Seek>(
    file: &mut ZipArchive<R>,
    limits: &VerificationLimits,
) -> Result<BTreeMap<String, Vec<u8>>, PackageError> {
    let mut entries = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut total_uncompressed = 0_u64;
    for index in 0..file.len() {
        let mut entry = file
            .by_index(index)
            .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
        let name = entry.name().to_owned();
        validate_path(&name)?;
        validate_entry_size(&name, entry.size(), &mut total_uncompressed, limits)?;
        if !seen.insert(name.clone()) {
            return Err(PackageError::DuplicatePath(name));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(PackageError::Symlink(name));
        }
        if entry.is_dir() {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| PackageError::InvalidArchive(error.to_string()))?;
        entries.insert(name, bytes);
    }
    Ok(entries)
}

fn read_archive(path: &Path, max: u64) -> Result<Vec<u8>, PackageError> {
    let mut file = File::open(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let actual = file
        .metadata()
        .map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if actual > max {
        return Err(PackageError::ArchiveTooLarge { actual, max });
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PackageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > max {
        return Err(PackageError::ArchiveTooLarge {
            actual: bytes.len() as u64,
            max,
        });
    }
    Ok(bytes)
}

fn validate_entry_size(
    path: &str,
    entry_size: u64,
    total: &mut u64,
    limits: &VerificationLimits,
) -> Result<(), PackageError> {
    if entry_size > limits.max_entry_bytes {
        return Err(PackageError::EntryTooLarge {
            path: path.to_owned(),
            actual: entry_size,
            max: limits.max_entry_bytes,
        });
    }
    *total = total
        .checked_add(entry_size)
        .ok_or(PackageError::UncompressedPackageTooLarge {
            actual: u64::MAX,
            max: limits.max_total_uncompressed_bytes,
        })?;
    if *total > limits.max_total_uncompressed_bytes {
        return Err(PackageError::UncompressedPackageTooLarge {
            actual: *total,
            max: limits.max_total_uncompressed_bytes,
        });
    }
    Ok(())
}

fn validate_central_directory_names(bytes: &[u8]) -> Result<(), PackageError> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const CENTRAL: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    let eocd = bytes
        .windows(EOCD.len())
        .rposition(|window| window == EOCD)
        .ok_or_else(|| PackageError::InvalidArchive("missing end-of-directory record".into()))?;
    let entry_count = read_u16(bytes, eocd + 10)? as usize;
    let mut offset = read_u32(bytes, eocd + 16)? as usize;
    if entry_count == u16::MAX as usize || offset == u32::MAX as usize {
        return Err(PackageError::InvalidArchive(
            "ZIP64 packages are not supported by schema 1".into(),
        ));
    }
    let mut names = HashSet::new();
    for _ in 0..entry_count {
        if bytes.get(offset..offset + 4) != Some(CENTRAL.as_slice()) {
            return Err(PackageError::InvalidArchive(
                "malformed central-directory entry".into(),
            ));
        }
        let name_len = read_u16(bytes, offset + 28)? as usize;
        let extra_len = read_u16(bytes, offset + 30)? as usize;
        let comment_len = read_u16(bytes, offset + 32)? as usize;
        let name_start = offset + 46;
        let name_end = name_start.checked_add(name_len).ok_or_else(|| {
            PackageError::InvalidArchive("central-directory path length overflow".into())
        })?;
        let name = std::str::from_utf8(bytes.get(name_start..name_end).ok_or_else(|| {
            PackageError::InvalidArchive("truncated central-directory path".into())
        })?)
        .map_err(|_| PackageError::InvalidArchive("non-UTF-8 package path".into()))?;
        if !names.insert(name.to_owned()) {
            return Err(PackageError::DuplicatePath(name.to_owned()));
        }
        offset = name_end
            .checked_add(extra_len + comment_len)
            .ok_or_else(|| PackageError::InvalidArchive("entry length overflow".into()))?;
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, PackageError> {
    let value: [u8; 2] = bytes
        .get(offset..offset + 2)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| PackageError::InvalidArchive("truncated ZIP metadata".into()))?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PackageError> {
    let value: [u8; 4] = bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(|| PackageError::InvalidArchive("truncated ZIP metadata".into()))?;
    Ok(u32::from_le_bytes(value))
}

pub(crate) fn validate_path(value: &str) -> Result<(), PackageError> {
    let invalid = value.is_empty()
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || value.contains('\0')
        || value.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.contains(':')
                || part.chars().any(char::is_control)
        });
    if invalid {
        return Err(PackageError::UnsafePath(value.to_owned()));
    }
    Ok(())
}

fn required_entry<'a>(
    entries: &'a BTreeMap<String, Vec<u8>>,
    path: &'static str,
) -> Result<&'a [u8], PackageError> {
    entries
        .get(path)
        .map(Vec::as_slice)
        .ok_or(PackageError::MissingEntry(path))
}

fn parse_canonical_manifest(bytes: &[u8]) -> Result<PluginManifest, PackageError> {
    let manifest: PluginManifest = serde_json::from_slice(bytes)
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
    let canonical = serde_json::to_vec(&manifest)
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
    if canonical != bytes {
        return Err(PackageError::InvalidManifest(
            "expected compact canonical field and map ordering".into(),
        ));
    }
    Ok(manifest)
}

fn verify_signature(
    manifest: &[u8],
    signature_bytes: &[u8],
    trusted_key: &VerifyingKey,
) -> Result<(), PackageError> {
    let signature = Signature::from_slice(signature_bytes)
        .map_err(|_| PackageError::InvalidSignatureEncoding(signature_bytes.len()))?;
    let payload = [SIGNATURE_DOMAIN, manifest].concat();
    trusted_key
        .verify(&payload, &signature)
        .map_err(|_| PackageError::SignatureVerification)
}

fn verify_file_tree(
    manifest: &PluginManifest,
    mut entries: BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>, PackageError> {
    entries.remove(MANIFEST_PATH);
    entries.remove(SIGNATURE_PATH);
    for path in entries.keys() {
        if !manifest.files.contains_key(path) {
            return Err(PackageError::UnsignedFile(path.clone()));
        }
    }
    for (path, expected) in &manifest.files {
        validate_path(path)?;
        let bytes = entries
            .get(path)
            .ok_or_else(|| PackageError::MissingFile(path.clone()))?;
        let actual = hex::encode(Sha256::digest(bytes));
        if expected != &actual {
            return Err(PackageError::HashMismatch {
                path: path.clone(),
                expected: expected.clone(),
                actual,
            });
        }
    }
    Ok(entries)
}
