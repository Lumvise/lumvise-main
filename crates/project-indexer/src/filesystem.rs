//! Filesystem adapter: scoped inventory, project ignore rules and one stable content read.

mod inventory;

use crate::source::{invalid, validate_path};
use crate::{FileRead, FileStamp, ProjectSource, ScanError, ScanScope, SourceEntry};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Reads one canonical project root. Example: `FilesystemProjectSource::open(".")`.
/// Ignores generated/build directories and honors project `.gitignore`/`.lumignore`
/// files. Symlinks are excluded; metadata shortcuts require Unix change timestamps.
pub struct FilesystemProjectSource {
    root: PathBuf,
}

impl FilesystemProjectSource {
    /// Opens the source boundary after validating its root.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ScanError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|error| path_error(root.as_ref(), error))?;
        if !root.is_dir() {
            return Err(invalid(
                root.display().to_string(),
                "expected project directory",
            ));
        }
        Ok(Self { root })
    }

    /// Returns the canonical root for publication identity.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl ProjectSource for FilesystemProjectSource {
    fn inventory(&self, scope: &ScanScope) -> Result<Vec<SourceEntry>, ScanError> {
        inventory::ProjectInventory::new(&self.root).collect(scope)
    }

    fn read_file(&self, path: &str) -> Result<FileRead, ScanError> {
        validate_path(path)?;
        let absolute = self.root.join(path);
        if has_symlink(&self.root, path)? {
            return Err(invalid(
                path,
                "expected regular project file without symlink ancestors",
            ));
        }
        read_stable_file(&absolute)
    }
}

fn read_stable_file(path: &Path) -> Result<FileRead, ScanError> {
    let mut file = File::open(path).map_err(|error| path_error(path, error))?;
    let before = file.metadata().map_err(|error| path_error(path, error))?;
    if !before.is_file() {
        return Err(path_error(path, "expected regular file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| path_error(path, error))?;
    let after = file.metadata().map_err(|error| path_error(path, error))?;
    if !stable_metadata(&before, &after) {
        return Err(path_error(
            path,
            "file changed during read; expected stable content, retry scan",
        ));
    }
    Ok(FileRead {
        bytes,
        stamp: metadata_stamp(&after),
    })
}

fn stable_metadata(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && metadata_stamp(before) == metadata_stamp(after)
}

fn metadata_stamp(metadata: &Metadata) -> Option<FileStamp> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let fields = [
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ];
        Some(FileStamp(
            fields.into_iter().flat_map(u64::to_le_bytes).collect(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn has_symlink(root: &Path, relative: &str) -> Result<bool, ScanError> {
    let mut current = root.to_path_buf();
    for part in relative.split('/') {
        current.push(part);
        match current.symlink_metadata() {
            Ok(metadata) if metadata.is_symlink() => return Ok(true),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(path_error(&current, error)),
        }
    }
    Ok(false)
}

fn path_error(path: &Path, error: impl std::fmt::Display) -> ScanError {
    invalid(
        path.display().to_string(),
        format!("{error}; expected readable project source"),
    )
}
