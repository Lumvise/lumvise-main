use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use lumvise_plugin_package::{ExportDescriptor, InstalledPlugin};
use sha2::{Digest, Sha256};

use crate::{
    PluginRuntimeError,
    process::PluginProcess,
    schema::{ExportValidators, compile_exports},
};

pub(crate) struct PluginEntry {
    plugin_id: String,
    exports: Vec<ExportDescriptor>,
    validators: HashMap<String, ExportValidators>,
    package_root: PathBuf,
    view_file_sha256: BTreeMap<String, String>,
    cataloged: AtomicBool,
    ready: AtomicBool,
    installation: Mutex<PluginInstallation>,
}

impl PluginEntry {
    pub(crate) fn from_package(package: &InstalledPlugin) -> Result<Self, PluginRuntimeError> {
        Ok(Self {
            plugin_id: package.plugin_id().to_owned(),
            exports: package.exports().to_vec(),
            validators: compile_exports(package)?,
            package_root: package.root().to_path_buf(),
            view_file_sha256: signed_view_files(package),
            cataloged: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            installation: Mutex::new(PluginInstallation::from_package(package)?),
        })
    }

    pub(crate) fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(crate) fn exports(&self) -> &[ExportDescriptor] {
        &self.exports
    }

    pub(crate) fn validators(&self, export_id: &str) -> Option<&ExportValidators> {
        self.validators.get(export_id)
    }

    pub(crate) fn package_root(&self) -> &Path {
        &self.package_root
    }

    pub(crate) fn view_file_sha256(&self, path: &str) -> Option<&str> {
        self.view_file_sha256.get(path).map(String::as_str)
    }

    pub(crate) fn is_cataloged(&self) -> bool {
        self.cataloged.load(Ordering::Acquire)
    }

    pub(crate) fn remove_from_catalog(&self) {
        self.cataloged.store(false, Ordering::Release);
        self.ready.store(false, Ordering::Release);
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub(crate) fn publish(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub(crate) fn unpublish(&self) {
        self.ready.store(false, Ordering::Release);
    }

    pub(crate) fn lifecycle(
        &self,
    ) -> Result<MutexGuard<'_, PluginInstallation>, PluginRuntimeError> {
        self.installation
            .lock()
            .map_err(|_| PluginRuntimeError::PluginStatePoisoned(self.plugin_id.clone()))
    }
}

pub(crate) struct PluginInstallation {
    package_digest: String,
    package_root: PathBuf,
    executable: PathBuf,
    executable_sha256: String,
    host_capabilities: HashMap<String, String>,
    /// Shared handle so concurrent invocations multiplex over one process pipe
    /// without holding the lifecycle mutex.
    pub(crate) process: Option<Arc<PluginProcess>>,
}

impl PluginInstallation {
    fn from_package(package: &InstalledPlugin) -> Result<Self, PluginRuntimeError> {
        let executable_sha256 = executable_sha256(package.executable())?;
        Ok(Self {
            package_digest: package.package_digest().to_owned(),
            package_root: package.root().to_path_buf(),
            executable: package.executable().to_path_buf(),
            executable_sha256,
            host_capabilities: package
                .host_capabilities()
                .iter()
                .map(|capability| (capability.id.clone(), capability.version.clone()))
                .collect(),
            process: None,
        })
    }

    pub(crate) fn package_digest(&self) -> &str {
        &self.package_digest
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn package_root(&self) -> &Path {
        &self.package_root
    }

    pub(crate) fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }

    pub(crate) fn host_capabilities(&self) -> &HashMap<String, String> {
        &self.host_capabilities
    }
}

fn signed_view_files(package: &InstalledPlugin) -> BTreeMap<String, String> {
    let views = package
        .exports()
        .iter()
        .filter_map(|export| match &export.surface {
            lumvise_plugin_package::ExportSurface::View { asset_path, .. } => {
                Some((view_asset_root(asset_path), asset_path.as_str()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    package
        .signed_files()
        .iter()
        .filter(|(path, _)| {
            views.iter().any(|(root, entry)| {
                (!root.is_empty() && path.starts_with(root)) || path.as_str() == *entry
            })
        })
        .map(|(path, digest)| (path.clone(), digest.clone()))
        .collect()
}

fn view_asset_root(entry_path: &str) -> String {
    entry_path
        .rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| format!("{parent}/"))
}

fn executable_sha256(path: &Path) -> Result<String, PluginRuntimeError> {
    let bytes = std::fs::read(path).map_err(|source| PluginRuntimeError::Spawn {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
