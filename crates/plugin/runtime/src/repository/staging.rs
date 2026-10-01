//! Filesystem staging and rollback for repository archive transitions.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use lumvise_plugin_package::InstalledPlugin;
use tempfile::NamedTempFile;

use super::{PluginRegistryRecord, PluginRepositoryError, invalid, io_error, safe_join};

pub(super) struct StagedRemoval {
    stage: tempfile::TempDir,
    extracted_original: PathBuf,
    extracted_staged: PathBuf,
    extracted_permissions: fs::Permissions,
    archive: Option<(PathBuf, PathBuf)>,
}

impl StagedRemoval {
    pub(super) fn stage(
        root: &Path,
        record: &PluginRegistryRecord,
        purge_archive: bool,
    ) -> Result<Self, PluginRepositoryError> {
        let stage = tempfile::Builder::new()
            .prefix(".removing-")
            .tempdir_in(root)
            .map_err(|source| io_error(root, source))?;
        let extracted_original = safe_join(root, &record.extracted_path)?;
        let extracted_staged = stage.path().join("extracted");
        let extracted_permissions = fs::metadata(&extracted_original)
            .map_err(|source| io_error(&extracted_original, source))?
            .permissions();
        make_root_movable(&extracted_original, &extracted_permissions)?;
        if let Err(source) = fs::rename(&extracted_original, &extracted_staged) {
            let _ = fs::set_permissions(&extracted_original, extracted_permissions.clone());
            return Err(io_error(&extracted_original, source));
        }
        if let Err(source) = fs::set_permissions(&extracted_staged, extracted_permissions.clone()) {
            let _ = restore_staged_extraction(
                &extracted_staged,
                &extracted_original,
                &extracted_permissions,
            );
            return Err(io_error(&extracted_staged, source));
        }
        let archive = if purge_archive {
            let original = safe_join(root, &record.archive_path)?;
            let staged = stage.path().join("archive.lvp");
            if let Err(source) = fs::rename(&original, &staged) {
                let _ = restore_staged_extraction(
                    &extracted_staged,
                    &extracted_original,
                    &extracted_permissions,
                );
                return Err(io_error(&original, source));
            }
            Some((original, staged))
        } else {
            None
        };
        Ok(Self {
            stage,
            extracted_original,
            extracted_staged,
            extracted_permissions,
            archive,
        })
    }

    pub(super) fn rollback(self) -> Result<(), PluginRepositoryError> {
        if let Some((original, staged)) = &self.archive {
            fs::rename(staged, original).map_err(|source| io_error(original, source))?;
        }
        restore_staged_extraction(
            &self.extracted_staged,
            &self.extracted_original,
            &self.extracted_permissions,
        )
    }

    pub(super) fn finish(self) {
        let path = self.stage.path().to_owned();
        let _ = make_writable(&path);
        let _ = self.stage.close();
    }
}

#[cfg(unix)]
fn make_root_movable(
    path: &Path,
    permissions: &fs::Permissions,
) -> Result<(), PluginRepositoryError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(permissions.mode() | 0o200))
        .map_err(|source| io_error(path, source))
}

#[cfg(not(unix))]
fn make_root_movable(
    path: &Path,
    permissions: &fs::Permissions,
) -> Result<(), PluginRepositoryError> {
    let mut writable = permissions.clone();
    writable.set_readonly(false);
    fs::set_permissions(path, writable).map_err(|source| io_error(path, source))
}

fn restore_staged_extraction(
    staged: &Path,
    original: &Path,
    permissions: &fs::Permissions,
) -> Result<(), PluginRepositoryError> {
    make_root_movable(staged, permissions)?;
    fs::rename(staged, original).map_err(|source| io_error(original, source))?;
    fs::set_permissions(original, permissions.clone()).map_err(|source| io_error(original, source))
}

pub(super) fn stage_archive(
    source: &Path,
    destination: &Path,
) -> Result<bool, PluginRepositoryError> {
    if destination.exists() {
        return Ok(false);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid(destination, "missing archive parent".into()))?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let mut staged = NamedTempFile::new_in(parent).map_err(|source| io_error(parent, source))?;
    let bytes = fs::read(source).map_err(|error| io_error(source, error))?;
    staged
        .write_all(&bytes)
        .map_err(|source| io_error(destination, source))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|source| io_error(destination, source))?;
    staged
        .persist_noclobber(destination)
        .map_err(|error| io_error(destination, error.error))?;
    Ok(true)
}

pub(super) fn rollback_install(installed: &InstalledPlugin, archive_created: bool, archive: &Path) {
    let _ = remove_tree(installed.root());
    rollback_archive(archive_created, archive);
}

pub(super) fn rollback_archive(created: bool, archive: &Path) {
    if created {
        let _ = fs::remove_file(archive);
    }
}

pub(super) fn remove_tree(path: &Path) -> Result<(), PluginRepositoryError> {
    if !path.exists() {
        return Ok(());
    }
    make_writable(path)?;
    fs::remove_dir_all(path).map_err(|source| io_error(path, source))
}

#[cfg(unix)]
fn make_writable(root: &Path) -> Result<(), PluginRepositoryError> {
    use std::os::unix::fs::PermissionsExt;
    for path in walk(root)? {
        let metadata = fs::metadata(&path).map_err(|source| io_error(&path, source))?;
        let mode = metadata.permissions().mode() | 0o700;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_writable(root: &Path) -> Result<(), PluginRepositoryError> {
    for path in walk(root)? {
        let mut permissions = fs::metadata(&path)
            .map_err(|source| io_error(&path, source))?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

fn walk(root: &Path) -> Result<Vec<PathBuf>, PluginRepositoryError> {
    let mut paths = Vec::new();
    if root.is_dir() {
        for item in fs::read_dir(root).map_err(|source| io_error(root, source))? {
            let path = item.map_err(|source| io_error(root, source))?.path();
            if path.is_dir() {
                paths.extend(walk(&path)?);
            }
            paths.push(path);
        }
    }
    paths.push(root.to_path_buf());
    Ok(paths)
}
