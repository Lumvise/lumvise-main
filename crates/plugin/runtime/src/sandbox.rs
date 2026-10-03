use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_os = "macos")]
use std::sync::Arc;

#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
};
#[cfg(target_os = "macos")]
use tempfile::TempDir;

#[cfg(target_os = "macos")]
mod document_assets;

/// Immutable signed identity supplied to an OS-specific process sandbox.
#[derive(Clone, Copy, Debug)]
pub struct PluginSandboxRequest<'package> {
    /// Signed plugin identity.
    pub plugin_id: &'package str,
    /// Signed canonical package digest.
    pub package_digest: &'package str,
    /// Root containing only verified package files.
    pub package_root: &'package Path,
    /// Verified immutable package executable.
    pub executable: &'package Path,
    /// SHA-256 identity captured when the verified package entered the catalog.
    pub executable_sha256: &'package str,
}

/// Structured sandbox policy or preparation failure.
#[derive(Debug, thiserror::Error)]
#[error("plugin sandbox rejected `{plugin_id}` with `{code}`: {message}")]
pub struct PluginSandboxError {
    /// Plugin denied execution.
    pub plugin_id: String,
    /// Stable machine-readable policy code.
    pub code: String,
    /// Human-readable policy diagnostic.
    pub message: String,
}

impl PluginSandboxError {
    /// Creates a structured sandbox failure.
    pub fn new(
        plugin_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

/// OS process-containment seam for compiled plugin execution.
pub trait PluginSandbox: Send + Sync {
    /// Creates a command with platform containment applied.
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError>;
}

/// Fail-closed sandbox for non-production construction and unsupported hosts.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyExecutionSandbox;

impl PluginSandbox for DenyExecutionSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        Err(PluginSandboxError::new(
            request.plugin_id,
            "sandbox_adapter_required",
            "compiled plugin execution requires a production OS sandbox",
        ))
    }
}

/// Current-platform production process containment.
///
/// macOS uses a default-deny Seatbelt profile. Verified executable bytes are
/// copied from their already-open file into a private immutable execution cache,
/// closing replacement races between identity verification and process spawn.
#[derive(Clone, Debug)]
pub struct ProductionPluginSandbox {
    #[cfg(target_os = "macos")]
    sandbox_program: PathBuf,
    #[cfg(target_os = "macos")]
    _execution_cache: Arc<TempDir>,
    #[cfg(target_os = "macos")]
    execution_root: PathBuf,
}

impl ProductionPluginSandbox {
    /// Validates and creates current-platform production containment.
    ///
    /// # Errors
    /// Fails closed when platform support, OS sandbox facility, or private cache
    /// creation is unavailable.
    pub fn new() -> Result<Self, PluginSandboxError> {
        platform_sandbox()
    }
}

impl PluginSandbox for ProductionPluginSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        #[cfg(target_os = "macos")]
        {
            prepare_platform_command(&self.sandbox_program, &self.execution_root, request)
        }
        #[cfg(not(target_os = "macos"))]
        {
            prepare_platform_command(request)
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_sandbox() -> Result<ProductionPluginSandbox, PluginSandboxError> {
    let sandbox_program = PathBuf::from("/usr/bin/sandbox-exec");
    require_regular_file(&sandbox_program)?;
    let execution_cache = tempfile::tempdir().map_err(|error| {
        PluginSandboxError::new(
            "<runtime>",
            "sandbox_cache_unavailable",
            format!("private executable cache creation failed: {error}"),
        )
    })?;
    let execution_root = execution_cache.path().canonicalize().map_err(|error| {
        PluginSandboxError::new(
            "<runtime>",
            "sandbox_cache_unavailable",
            format!("private executable cache canonicalization failed: {error}"),
        )
    })?;
    Ok(ProductionPluginSandbox {
        sandbox_program,
        _execution_cache: Arc::new(execution_cache),
        execution_root,
    })
}

#[cfg(target_os = "macos")]
fn require_regular_file(path: &Path) -> Result<(), PluginSandboxError> {
    let metadata = path.metadata().map_err(|error| {
        PluginSandboxError::new(
            "<runtime>",
            "sandbox_dependency_unavailable",
            format!("required `{}` is unavailable: {error}", path.display()),
        )
    })?;
    if metadata.is_file() {
        return Ok(());
    }
    Err(PluginSandboxError::new(
        "<runtime>",
        "sandbox_dependency_unavailable",
        format!("required `{}` is not a regular file", path.display()),
    ))
}

#[cfg(not(target_os = "macos"))]
fn platform_sandbox() -> Result<ProductionPluginSandbox, PluginSandboxError> {
    Ok(ProductionPluginSandbox {})
}

#[cfg(target_os = "macos")]
fn prepare_platform_command(
    sandbox_program: &Path,
    execution_cache: &Path,
    request: PluginSandboxRequest<'_>,
) -> Result<Command, PluginSandboxError> {
    let mut source = open_executable(request)?;
    verify_executable_identity(&mut source, request)?;
    let cached = cache_executable(&mut source, execution_cache, request)?;
    let package_root = canonical_package_root(request)?;
    let mut command = Command::new(sandbox_program);
    let assets = document_assets::DocumentModelAssets::for_plugin(request.plugin_id, |name| {
        std::env::var_os(name)
    });
    command
        .env_clear()
        .current_dir(&package_root)
        .arg("-D")
        .arg(format!("PACKAGE_ROOT={}", package_root.display()))
        .arg("-D")
        .arg(format!("EXECUTION_ROOT={}", execution_cache.display()));
    assets.apply(&mut command, MACOS_SANDBOX_PROFILE);
    command.arg(cached);
    Ok(command)
}

fn canonical_package_root(
    request: PluginSandboxRequest<'_>,
) -> Result<PathBuf, PluginSandboxError> {
    request.package_root.canonicalize().map_err(|error| {
        PluginSandboxError::new(
            request.plugin_id,
            "package_root_unavailable",
            format!("`{}` failed: {error}", request.package_root.display()),
        )
    })
}

#[cfg(not(target_os = "macos"))]
fn prepare_platform_command(
    request: PluginSandboxRequest<'_>,
) -> Result<Command, PluginSandboxError> {
    let package_root = canonical_package_root(request)?;
    let mut command = Command::new(request.executable);
    command.current_dir(package_root);
    Ok(command)
}

#[cfg(target_os = "macos")]
fn open_executable(request: PluginSandboxRequest<'_>) -> Result<File, PluginSandboxError> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(request.executable)
        .map_err(|error| executable_error(request, "executable_open_failed", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| executable_error(request, "executable_metadata_failed", error))?;
    if metadata.is_file() {
        return Ok(file);
    }
    Err(PluginSandboxError::new(
        request.plugin_id,
        "executable_not_regular",
        format!("`{}` is not a regular file", request.executable.display()),
    ))
}

#[cfg(target_os = "macos")]
fn verify_executable_identity(
    executable: &mut File,
    request: PluginSandboxRequest<'_>,
) -> Result<(), PluginSandboxError> {
    let actual = file_sha256(executable)
        .map_err(|error| executable_error(request, "executable_identity_read_failed", error))?;
    executable
        .rewind()
        .map_err(|error| executable_error(request, "executable_identity_read_failed", error))?;
    if actual == request.executable_sha256 {
        return Ok(());
    }
    Err(PluginSandboxError::new(
        request.plugin_id,
        "executable_identity_mismatch",
        format!(
            "`{}` SHA-256 was `{actual}`, expected `{}`",
            request.executable.display(),
            request.executable_sha256
        ),
    ))
}

#[cfg(target_os = "macos")]
fn cache_executable(
    source: &mut File,
    cache_root: &Path,
    request: PluginSandboxRequest<'_>,
) -> Result<PathBuf, PluginSandboxError> {
    let relative = request
        .executable
        .strip_prefix(request.package_root)
        .map_err(|_| {
            PluginSandboxError::new(
                request.plugin_id,
                "executable_outside_package",
                format!("`{}` is outside package root", request.executable.display()),
            )
        })?;
    let destination = cache_root
        .join(request.plugin_id)
        .join(request.package_digest)
        .join(relative);
    publish_cached_executable(source, &destination, request)?;
    Ok(destination)
}

#[cfg(target_os = "macos")]
fn publish_cached_executable(
    source: &mut File,
    destination: &Path,
    request: PluginSandboxRequest<'_>,
) -> Result<(), PluginSandboxError> {
    if destination.exists() {
        return verify_cached_executable(destination, request);
    }
    let parent = destination.parent().ok_or_else(|| {
        PluginSandboxError::new(
            request.plugin_id,
            "sandbox_cache_invalid",
            "missing cache parent",
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| cache_error(request, destination, error))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| cache_error(request, destination, error))?;
    copy_executable(source, &mut staged, destination, request)?;
    staged
        .persist_noclobber(destination)
        .map_err(|error| cache_error(request, destination, error.error))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn copy_executable(
    source: &mut File,
    staged: &mut tempfile::NamedTempFile,
    destination: &Path,
    request: PluginSandboxRequest<'_>,
) -> Result<(), PluginSandboxError> {
    use std::os::unix::fs::PermissionsExt;

    source
        .rewind()
        .and_then(|()| std::io::copy(source, staged.as_file_mut()).map(|_| ()))
        .and_then(|()| staged.flush())
        .and_then(|()| staged.as_file().sync_all())
        .and_then(|()| {
            staged
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o500))
        })
        .map_err(|error| cache_error(request, destination, error))
}

#[cfg(target_os = "macos")]
fn verify_cached_executable(
    path: &Path,
    request: PluginSandboxRequest<'_>,
) -> Result<(), PluginSandboxError> {
    let mut file = File::open(path).map_err(|error| cache_error(request, path, error))?;
    let actual = file_sha256(&mut file).map_err(|error| cache_error(request, path, error))?;
    if actual == request.executable_sha256 {
        return Ok(());
    }
    Err(PluginSandboxError::new(
        request.plugin_id,
        "sandbox_cache_identity_mismatch",
        format!("cached `{}` SHA-256 was `{actual}`", path.display()),
    ))
}

#[cfg(target_os = "macos")]
fn file_sha256(file: &mut File) -> std::io::Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex::encode(digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

#[cfg(target_os = "macos")]
fn executable_error(
    request: PluginSandboxRequest<'_>,
    code: &str,
    error: std::io::Error,
) -> PluginSandboxError {
    PluginSandboxError::new(
        request.plugin_id,
        code,
        format!("`{}` failed: {error}", request.executable.display()),
    )
}

#[cfg(target_os = "macos")]
fn cache_error(
    request: PluginSandboxRequest<'_>,
    path: &Path,
    error: std::io::Error,
) -> PluginSandboxError {
    PluginSandboxError::new(
        request.plugin_id,
        "sandbox_cache_failed",
        format!("cache `{}` failed: {error}", path.display()),
    )
}

#[cfg(target_os = "macos")]
const MACOS_SANDBOX_PROFILE: &str = r#"
(version 1)
(deny default)
(allow process-exec (subpath (param "EXECUTION_ROOT")))
(allow signal (target self))
(allow sysctl-read)
(allow file-read*)
(deny file-read*
    (subpath "/Users")
    (subpath "/home")
    (subpath "/root")
    (subpath "/Volumes")
    (subpath "/Network")
    (subpath "/private/etc")
    (subpath "/private/var"))
(allow file-read*
    (subpath (param "PACKAGE_ROOT"))
    (subpath (param "EXECUTION_ROOT"))
    (literal "/dev/null")
    (literal "/dev/urandom"))
(allow file-write* (literal "/dev/null"))
"#;

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn production_plugin_sandbox_direct_launch_uses_package_root() {
        let package_root = tempfile::tempdir().expect("temporary package root");
        let package_root = package_root
            .path()
            .canonicalize()
            .expect("canonical package root");
        let executable = package_root.join("plugin.exe");
        let request = PluginSandboxRequest {
            plugin_id: "trusted-plugin",
            package_digest: "package-digest",
            package_root: &package_root,
            executable: &executable,
            executable_sha256: "unused-on-non-macos",
        };

        let sandbox = ProductionPluginSandbox::new().expect("production sandbox");
        let command = sandbox
            .prepare_command(request)
            .expect("direct launch command");

        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(command.get_current_dir(), Some(package_root.as_path()));
    }
}
