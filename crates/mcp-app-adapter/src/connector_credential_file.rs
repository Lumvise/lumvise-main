//! Private installation credential file, selected by connector configuration rather than PID or label.
//! CLI setup calls `load_or_create`; no proof is exposed as an MCP argument or diagnostic.
use lumvise_contracts::ConnectorCredential;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) fn load_or_create(
    path: &Path,
    generate: impl FnOnce() -> Result<ConnectorCredential, String>,
) -> Result<ConnectorCredential, String> {
    #[cfg(not(unix))]
    return Err(format!(
        "connector credential file {path:?}; expected a supported protected credential store (currently Unix)"
    ));
    let parent = path
        .parent()
        .ok_or_else(|| format!("credential file {path:?}; expected a private parent directory"))?;
    create_private_directory(parent)?;
    let mut file = open_private_file(path)?;
    file.lock().map_err(|error| {
        format!("credential file {path:?}; expected an exclusive setup lock: {error}")
    })?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| format!("credential file {path:?}; expected readable proof: {error}"))?;
    if text.is_empty() {
        return initialize(&mut file, parent, generate()?);
    }
    valid_proof(text)
}

fn initialize(
    file: &mut File,
    parent: &Path,
    credential: ConnectorCredential,
) -> Result<ConnectorCredential, String> {
    let credential = valid_proof(credential.0)?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(credential.0.as_bytes()))
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            format!("connector credential publication; expected durable private file: {error}")
        })?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!("connector credential directory {parent:?}; expected durable entry: {error}")
        })?;
    Ok(credential)
}

fn valid_proof(value: String) -> Result<ConnectorCredential, String> {
    let credential = ConnectorCredential(value);
    credential.validate()?;
    Ok(credential)
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        format!("credential directory {path:?}; expected private directory: {error}")
    })?;
    require_private(
        path,
        &fs::symlink_metadata(path).map_err(|e| e.to_string())?,
        true,
    )
}

fn open_private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| {
        format!("credential file {path:?}; expected private regular file: {error}")
    })?;
    require_private(path, &file.metadata().map_err(|e| e.to_string())?, false)?;
    Ok(file)
}

fn require_private(path: &Path, metadata: &fs::Metadata, directory: bool) -> Result<(), String> {
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(format!(
            "credential path {path:?}; expected regular private {}",
            if directory { "directory" } else { "file" }
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "credential path {path:?}; expected current-user ownership and no group/other permissions"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_file_keeps_identity_across_reconnect_and_never_replaces_bad_proof() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("private/connector");
        let first = load_or_create(&path, || Ok(ConnectorCredential("a1".repeat(32)))).unwrap();
        let again =
            load_or_create(&path, || Err("must not regenerate an installation".into())).unwrap();
        assert_eq!(first, again);
        fs::write(&path, "invalid").unwrap();
        assert!(load_or_create(&path, || Ok(ConnectorCredential("b2".repeat(32)))).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "invalid");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_publicly_readable_credentials_are_refused_without_exposing_secrets() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("private/connector");
        let secret = "c3".repeat(32);
        load_or_create(&path, || Ok(ConnectorCredential(secret.clone()))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = load_or_create(&path, || unreachable!()).unwrap_err();
        assert!(!error.contains(&secret));
        let link = path.with_file_name("link");
        symlink(&path, &link).unwrap();
        assert!(load_or_create(&link, || unreachable!()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), secret);
    }
}
