use std::{fs, path::Path};

use ed25519_dalek::SigningKey;

use crate::PackageError;

/// Reads an offline Ed25519 signing key from a protected regular file.
pub fn read_protected_signing_key(path: &Path) -> Result<SigningKey, PackageError> {
    require_private_key_file(path)?;
    let mut bytes = fs::read(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let secret = decode_secret(&bytes, path);
    bytes.fill(0);
    let mut secret = secret?;
    let key = SigningKey::from_bytes(&secret);
    secret.fill(0);
    Ok(key)
}

fn decode_secret(bytes: &[u8], path: &Path) -> Result<[u8; 32], PackageError> {
    if let Ok(secret) = <[u8; 32]>::try_from(bytes) {
        return Ok(secret);
    }
    std::str::from_utf8(bytes).ok().map(str::trim)
        .and_then(|text| hex::decode(text).ok())
        .and_then(|value| <[u8; 32]>::try_from(value.as_slice()).ok())
        .ok_or_else(|| PackageError::InvalidManifest(format!(
            "signing key `{}` has invalid bytes; expected 32 raw bytes or 64 hexadecimal characters",
            path.display())))
}

#[cfg(unix)]
fn require_private_key_file(path: &Path) -> Result<(), PackageError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.is_file() && mode & 0o077 == 0 {
        return Ok(());
    }
    Err(PackageError::InvalidManifest(format!(
        "signing key `{}` has mode {mode:o}; expected no group/other permissions on a protected regular file",
        path.display()
    )))
}

#[cfg(not(unix))]
fn require_private_key_file(path: &Path) -> Result<(), PackageError> {
    if metadata(path)?.is_file() {
        return Ok(());
    }
    Err(PackageError::InvalidManifest(format!(
        "signing key `{}` is not a regular file; expected protected key file",
        path.display()
    )))
}

fn metadata(path: &Path) -> Result<fs::Metadata, PackageError> {
    fs::metadata(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })
}
