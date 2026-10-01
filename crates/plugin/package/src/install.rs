use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use tempfile::Builder;

use crate::{
    ExportDescriptor, HostCapabilityRequirement, PackageError, archive::VerifiedPackageContents,
};

/// Paths and identity of one installed immutable plugin version.
#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    plugin_id: String,
    plugin_version: String,
    package_digest: String,
    root: PathBuf,
    executable: PathBuf,
    exports: Vec<ExportDescriptor>,
    host_capabilities: Vec<HostCapabilityRequirement>,
    files: BTreeMap<String, String>,
}

impl InstalledPlugin {
    /// Returns the stable plugin identity.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Returns the installed semantic version.
    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    /// Returns the lowercase SHA-256 digest of the canonical signed manifest.
    pub fn package_digest(&self) -> &str {
        &self.package_digest
    }

    /// Returns the immutable installed package root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the host-selected executable path.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns the signed generic export catalog.
    pub fn exports(&self) -> &[ExportDescriptor] {
        &self.exports
    }

    /// Returns the signed Host Capability requests.
    pub fn host_capabilities(&self) -> &[HostCapabilityRequirement] {
        &self.host_capabilities
    }

    /// Returns signed SHA-256 digest for one canonical package-relative file.
    pub fn file_sha256(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }

    /// Returns the canonical signed file tree retained by the installed package.
    ///
    /// Runtime adapters use this map to expose only package files covered by the
    /// verified manifest. Paths are canonical package-relative slash paths.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// assert!(installed.signed_files().contains_key("views/index.html"));
    /// ```
    pub fn signed_files(&self) -> &BTreeMap<String, String> {
        &self.files
    }
}

pub(crate) fn install(
    package: &VerifiedPackageContents,
    install_root: &Path,
) -> Result<InstalledPlugin, PackageError> {
    fs::create_dir_all(install_root).map_err(|source| io_error(install_root, source))?;
    let plugin_root = install_root.join(&package.manifest.plugin_id);
    fs::create_dir_all(&plugin_root).map_err(|source| io_error(&plugin_root, source))?;
    let destination = plugin_root.join(&package.manifest.plugin_version);
    if destination.exists() {
        return Err(PackageError::AlreadyInstalled(destination));
    }

    let stage = Builder::new()
        .prefix(".staging-")
        .tempdir_in(&plugin_root)
        .map_err(|source| io_error(&plugin_root, source))?;
    write_files(stage.path(), package)?;
    make_tree_immutable(stage.path(), &package.executable_path)?;
    let stage_path = stage.keep();
    if destination.exists() {
        remove_stage(&stage_path);
        return Err(PackageError::AlreadyInstalled(destination));
    }
    if let Err(source) = fs::rename(&stage_path, &destination) {
        remove_stage(&stage_path);
        return Err(io_error(&destination, source));
    }
    make_root_immutable(&destination)?;

    Ok(InstalledPlugin {
        plugin_id: package.manifest.plugin_id.clone(),
        plugin_version: package.manifest.plugin_version.clone(),
        package_digest: package.package_digest.clone(),
        exports: package.manifest.exports.clone(),
        host_capabilities: package.manifest.host_capabilities.clone(),
        files: package.manifest.files.clone(),
        executable: destination.join(&package.executable_path),
        root: destination,
    })
}

pub(crate) fn remove_installed(package: &InstalledPlugin) -> Result<(), PackageError> {
    make_tree_writable(package.root())?;
    fs::remove_dir_all(package.root()).map_err(|source| io_error(package.root(), source))
}

fn remove_stage(path: &Path) {
    let _ = make_tree_writable(path);
    let _ = fs::remove_dir_all(path);
}

#[cfg(unix)]
fn make_tree_writable(root: &Path) -> Result<(), PackageError> {
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
fn make_tree_writable(root: &Path) -> Result<(), PackageError> {
    for path in walk(root)? {
        let mut permissions = fs::metadata(&path)
            .map_err(|source| io_error(&path, source))?
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

fn write_files(root: &Path, package: &VerifiedPackageContents) -> Result<(), PackageError> {
    for (relative, bytes) in &package.files {
        let path = root.join(relative);
        let parent = path
            .parent()
            .ok_or_else(|| PackageError::UnsafePath(relative.clone()))?;
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        fs::write(&path, bytes).map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_tree_immutable(root: &Path, executable: &str) -> Result<(), PackageError> {
    use std::os::unix::fs::PermissionsExt;

    for entry in walk(root)? {
        if entry == root {
            continue;
        }
        let metadata = fs::metadata(&entry).map_err(|source| io_error(&entry, source))?;
        let mode = if metadata.is_dir() || entry == root.join(executable) {
            0o555
        } else {
            0o444
        };
        fs::set_permissions(&entry, fs::Permissions::from_mode(mode))
            .map_err(|source| io_error(&entry, source))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_tree_immutable(root: &Path, _executable: &str) -> Result<(), PackageError> {
    for entry in walk(root)? {
        if entry == root {
            continue;
        }
        let mut permissions = fs::metadata(&entry)
            .map_err(|source| io_error(&entry, source))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&entry, permissions).map_err(|source| io_error(&entry, source))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_root_immutable(root: &Path) -> Result<(), PackageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root, fs::Permissions::from_mode(0o555))
        .map_err(|source| io_error(root, source))
}

#[cfg(not(unix))]
fn make_root_immutable(root: &Path) -> Result<(), PackageError> {
    let mut permissions = fs::metadata(root)
        .map_err(|source| io_error(root, source))?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(root, permissions).map_err(|source| io_error(root, source))
}

fn walk(root: &Path) -> Result<Vec<PathBuf>, PackageError> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).map_err(|source| io_error(root, source))? {
        let path = entry.map_err(|source| io_error(root, source))?.path();
        if path.is_dir() {
            found.extend(walk(&path)?);
        }
        found.push(path);
    }
    found.push(root.to_path_buf());
    Ok(found)
}

fn io_error(path: &Path, source: std::io::Error) -> PackageError {
    PackageError::Io {
        path: path.to_path_buf(),
        source,
    }
}
