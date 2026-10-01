//! Verifies and immutably stages compiled Lumvise plugin packages.
//!
//! Callers provide the trusted signing key and host compatibility. Archive,
//! cryptographic, and filesystem details remain private to this crate.

#![deny(missing_docs)]

mod archive;
mod builder;
mod directory_builder;
mod error;
mod install;
mod manifest;
mod policy;
mod release_index;
mod signing_key;
pub use release_index::{
    PLUGIN_RELEASE_INDEX_SCHEMA_VERSION, PluginReleaseArtifactV2, PluginReleaseIndexError,
    PluginReleaseIndexV2, ReleaseComposition,
};
mod trust;

use std::path::Path;

use ed25519_dalek::VerifyingKey;

pub use builder::{BuildPackageRequest, build_package};
pub use directory_builder::build_package_from_directory;
pub use error::PackageError;
pub use install::InstalledPlugin;
pub use manifest::{
    BackgroundDeliveryPolicy, ExclusiveLaneOperation, ExclusiveLanePolicy, ExecutionMode,
    ExportDescriptor, ExportSurface, HostCapabilityRequirement, HostCompatibility, HttpMethod,
    HttpStreamMode, InvocationAdmissionPolicy, PluginManifest, ProtocolRange, PublisherIdentity,
    SseStreamPolicy, ViewMenuPlacement, ViewSurface,
};
pub use policy::VerificationLimits;
pub use signing_key::read_protected_signing_key;
pub use trust::PublisherTrustStore;

use archive::VerifiedPackageContents;

/// A package whose identity, file tree, target, and signature were verified.
///
/// Its file bytes are held privately so installation cannot re-read a changed
/// archive after verification.
pub struct VerifiedPackage(VerifiedPackageContents);

impl VerifiedPackage {
    /// Returns the fully verified signed manifest.
    pub fn manifest(&self) -> &PluginManifest {
        &self.0.manifest
    }

    /// Returns the signed generic export catalog.
    pub fn exports(&self) -> &[ExportDescriptor] {
        &self.0.manifest.exports
    }

    /// Returns the signed Host Capability requests.
    pub fn host_capabilities(&self) -> &[HostCapabilityRequirement] {
        &self.0.manifest.host_capabilities
    }
}

/// Verifies one `.lvp` package against an explicitly supplied tooling key.
///
/// Production installation should use [`PublisherTrustStore::verify_package`]
/// so signed publisher identity, key binding, and revocation are enforced.
///
/// # Errors
/// Returns [`PackageError`] for malformed, unsafe, incompatible, corrupted, or
/// incorrectly signed packages.
pub fn verify_package(
    package_path: &Path,
    trusted_key: &VerifyingKey,
    host: &HostCompatibility,
) -> Result<VerifiedPackage, PackageError> {
    verify_package_with_limits(
        package_path,
        trusted_key,
        host,
        &VerificationLimits::default(),
    )
}

/// Verifies one `.lvp` package with explicit resource limits.
///
/// # Errors
/// Returns [`PackageError`] before extraction or entry allocation when the
/// compressed archive, one entry, or the total uncompressed tree exceeds its
/// configured maximum. Other verification failures match [`verify_package`].
pub fn verify_package_with_limits(
    package_path: &Path,
    trusted_key: &VerifyingKey,
    host: &HostCompatibility,
    limits: &VerificationLimits,
) -> Result<VerifiedPackage, PackageError> {
    archive::verify(package_path, trusted_key, host, limits).map(VerifiedPackage)
}

/// Installs a verified package under `<root>/<plugin-id>/<version>`.
///
/// The completed tree is read-only and published with a no-clobber rename.
///
/// # Errors
/// Returns [`PackageError`] when staging fails or the version is installed.
pub fn install_verified_package(
    package: &VerifiedPackage,
    install_root: &Path,
) -> Result<InstalledPlugin, PackageError> {
    install::install(&package.0, install_root)
}

/// Removes one immutable installed plugin version.
///
/// The installer temporarily restores owner permissions before deleting the
/// tree, so validation and uninstall workflows do not leak read-only files.
///
/// # Errors
/// Returns [`PackageError`] when permissions cannot be restored or the
/// installed version cannot be removed.
///
/// # Examples
/// ```ignore
/// let installed = install_verified_package(&verified, install_root)?;
/// remove_installed_package(&installed)?;
/// # Ok::<(), lumvise_plugin_package::PackageError>(())
/// ```
pub fn remove_installed_package(package: &InstalledPlugin) -> Result<(), PackageError> {
    install::remove_installed(package)
}
