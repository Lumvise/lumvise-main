//! Signed View asset verification and View Host API authorization.

use std::{io::Read, path::Path};

use lumvise_plugin_package::{ExportSurface, ViewSurface};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{PluginSystem, next_id, require_cataloged};
use crate::{
    PluginInvocationContext, PluginInvocationError, PluginRuntimeError, catalog::PluginEntry,
};

/// Verified bytes and signed renderer policy for one ready compiled View.
#[derive(Clone, Debug)]
pub struct PluginViewAsset {
    /// Stable plugin identity.
    pub plugin_id: String,
    /// Signed View identity.
    pub view_id: String,
    /// Requested renderer placement.
    pub surface: ViewSurface,
    /// Canonical package-relative asset path.
    pub asset_path: String,
    /// Verified asset bytes.
    pub bytes: Vec<u8>,
    /// Signed Content Security Policy.
    pub content_security_policy: String,
    /// Signed Host API allowlist.
    pub allowed_host_apis: Vec<String>,
}

impl PluginSystem {
    /// Reads one signed asset from a declared View subtree while its plugin is ready.
    ///
    /// An empty relative path selects the signed View entry asset. Every path
    /// component is opened without following symbolic links, and exact bytes are
    /// rehashed against signed package metadata on every read.
    ///
    /// # Examples
    /// ```ignore
    /// let html = system.read_view_asset("example.dashboard", "summary", "")?;
    /// let css = system.read_view_asset(
    ///     "builtin.assistant",
    ///     "summary",
    ///     "summary.css",
    /// )?;
    /// # Ok::<(), lumvise_plugin_runtime::PluginRuntimeError>(())
    /// ```
    pub fn read_view_asset(
        &self,
        plugin_id: &str,
        view_id: &str,
        relative_path: &str,
    ) -> Result<PluginViewAsset, PluginRuntimeError> {
        let entry = self.entry(plugin_id)?;
        let view = declared_view(&entry, plugin_id, view_id)?;
        require_cataloged(&entry)?;
        if !entry.is_ready() {
            return Err(PluginRuntimeError::NotReady(plugin_id.to_owned()));
        }
        let asset_path =
            resolve_view_asset_path(&view.asset_path, relative_path).ok_or_else(|| {
                view_asset_error(plugin_id, view_id, relative_path, "unsafe asset path")
            })?;
        let expected = entry.view_file_sha256(&asset_path).ok_or_else(|| {
            view_asset_error(
                plugin_id,
                view_id,
                &asset_path,
                "asset is outside the signed View subtree",
            )
        })?;
        let bytes = read_package_file(entry.package_root(), &asset_path).map_err(|error| {
            view_asset_error(plugin_id, view_id, &asset_path, error.to_string())
        })?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != expected {
            return Err(view_asset_error(
                plugin_id,
                view_id,
                &asset_path,
                format!("digest `{actual}`; expected `{expected}`"),
            ));
        }
        Ok(PluginViewAsset {
            plugin_id: plugin_id.to_owned(),
            view_id: view_id.to_owned(),
            surface: view.surface,
            asset_path,
            bytes,
            content_security_policy: view.content_security_policy,
            allowed_host_apis: view.allowed_host_apis,
        })
    }

    /// Invokes one View Host API through Runtime-owned lifecycle and admission.
    ///
    /// # Errors
    /// Returns typed admission, cancellation, availability, authorization, and host failures.
    pub fn invoke_view_host_api_controlled(
        &self,
        plugin_id: &str,
        view_id: &str,
        api_id: &str,
        input: Value,
        context: PluginInvocationContext,
    ) -> Result<Value, PluginInvocationError> {
        let active = self
            .active_invocations
            .register(plugin_id, &context)
            .map_err(PluginInvocationError::from_runtime)?;
        let result = self.invoke_view_host_api_inner(plugin_id, view_id, api_id, input, &context);
        drop(active);
        result.map_err(PluginInvocationError::from_runtime)
    }

    fn invoke_view_host_api_inner(
        &self,
        plugin_id: &str,
        view_id: &str,
        api_id: &str,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value, PluginRuntimeError> {
        context.ensure_active(plugin_id, self.config.invocation_deadline())?;
        let entry = self.entry(plugin_id)?;
        let view = declared_view(&entry, plugin_id, view_id)?;
        if !view
            .allowed_host_apis
            .iter()
            .any(|allowed| allowed == api_id)
        {
            return Err(view_api_denied(plugin_id, view_id, api_id));
        }
        let required_version = view_host_capability_version(&entry, plugin_id, view_id, api_id)?;
        let _permit = self.invocation_admission.admit(
            plugin_id,
            api_id,
            context,
            &self.config.export_concurrency,
            self.config.invocation_deadline(),
        )?;
        let output = self
            .broker
            .invoke(crate::HostCapabilityRequest {
                plugin_id: plugin_id.to_owned(),
                invocation_id: format!("view:{view_id}"),
                call_id: next_id("view-call"),
                capability_id: api_id.to_owned(),
                required_version,
                input,
            })
            .map_err(|error| PluginRuntimeError::ViewHostApiFailed {
                plugin_id: plugin_id.to_owned(),
                view_id: view_id.to_owned(),
                api_id: api_id.to_owned(),
                message: error.to_string(),
            })?;
        context.ensure_active(plugin_id, self.config.invocation_deadline())?;
        Ok(output)
    }
}

fn view_host_capability_version(
    entry: &PluginEntry,
    plugin_id: &str,
    view_id: &str,
    api_id: &str,
) -> Result<String, PluginRuntimeError> {
    let installation = entry.lifecycle()?;
    require_cataloged(entry)?;
    if !entry.is_ready() {
        return Err(PluginRuntimeError::NotReady(plugin_id.to_owned()));
    }
    installation
        .host_capabilities()
        .get(api_id)
        .cloned()
        .ok_or_else(|| view_api_denied(plugin_id, view_id, api_id))
}

fn resolve_view_asset_path(entry_path: &str, relative_path: &str) -> Option<String> {
    if relative_path.is_empty() {
        return Some(entry_path.to_owned());
    }
    if !is_safe_relative_asset_path(relative_path) {
        return None;
    }
    let parent = entry_path.rsplit_once('/').map(|(parent, _)| parent)?;
    Some(format!("{parent}/{relative_path}"))
}

fn is_safe_relative_asset_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(unix)]
fn read_package_file(root: &Path, relative_path: &str) -> std::io::Result<Vec<u8>> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let root = CString::new(root.as_os_str().as_bytes()).map_err(invalid_path_error)?;
    // SAFETY: `root` is a live NUL-terminated CString and flags do not expose memory.
    let descriptor = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful `open` returned this uniquely owned descriptor.
    let mut current = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let components = relative_path.split('/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let component = CString::new(*component).map_err(invalid_path_error)?;
        let final_component = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_component {
                0
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: both descriptors and CString pointers remain valid for this call.
        let descriptor = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful `openat` returned this uniquely owned descriptor.
        current = unsafe { OwnedFd::from_raw_fd(descriptor) };
    }
    read_regular_file(current)
}

#[cfg(unix)]
fn read_regular_file(descriptor: std::os::fd::OwnedFd) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::from(descriptor);
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "View asset is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(unix)]
fn invalid_path_error(error: std::ffi::NulError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
}

#[cfg(not(unix))]
fn read_package_file(root: &Path, relative_path: &str) -> std::io::Result<Vec<u8>> {
    let path = root.join(relative_path);
    let canonical_root = root.canonicalize()?;
    let canonical_path = path.canonicalize()?;
    if !canonical_path.starts_with(&canonical_root)
        || path.symlink_metadata()?.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "View asset escapes its immutable package root",
        ));
    }
    std::fs::read(canonical_path)
}

struct DeclaredView {
    surface: ViewSurface,
    asset_path: String,
    content_security_policy: String,
    allowed_host_apis: Vec<String>,
}

fn declared_view(
    entry: &PluginEntry,
    plugin_id: &str,
    view_id: &str,
) -> Result<DeclaredView, PluginRuntimeError> {
    entry
        .exports()
        .iter()
        .find_map(|export| match &export.surface {
            ExportSurface::View {
                view_id: declared_id,
                surface,
                asset_path,
                content_security_policy,
                allowed_host_apis,
                ..
            } if declared_id == view_id => Some(DeclaredView {
                surface: *surface,
                asset_path: asset_path.clone(),
                content_security_policy: content_security_policy.clone(),
                allowed_host_apis: allowed_host_apis.clone(),
            }),
            _ => None,
        })
        .ok_or_else(|| PluginRuntimeError::ViewNotFound {
            plugin_id: plugin_id.to_owned(),
            view_id: view_id.to_owned(),
        })
}

fn view_api_denied(plugin_id: &str, view_id: &str, api_id: &str) -> PluginRuntimeError {
    PluginRuntimeError::ViewHostApiDenied {
        plugin_id: plugin_id.to_owned(),
        view_id: view_id.to_owned(),
        api_id: api_id.to_owned(),
    }
}

fn view_asset_error(
    plugin_id: &str,
    view_id: &str,
    path: &str,
    message: impl Into<String>,
) -> PluginRuntimeError {
    PluginRuntimeError::ViewAssetInvalid {
        plugin_id: plugin_id.to_owned(),
        view_id: view_id.to_owned(),
        path: path.to_owned(),
        message: message.into(),
    }
}
