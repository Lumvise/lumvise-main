//! Deterministic source-independent release orchestration for Lumvise built-in plugins.

#![deny(missing_docs)]

mod compilation;
mod descriptors;
mod matrix;
mod publication;

pub use compilation::{
    BuiltinCompilationProfile, build_and_publish_builtin_release_composition_with_profile,
    build_builtin_binaries, build_builtin_binaries_composition_with_profile,
    build_builtin_binaries_with_profile,
};
pub use descriptors::{
    BuiltinReleaseDescriptor, discover_descriptors, discover_release_descriptors,
};
pub use matrix::{BuiltinBinaryPaths, BuiltinReleaseMatrix};
pub use publication::publish_builtin_release_composition;

/// Release orchestration failure with the offending path or matrix value.
#[derive(Debug, thiserror::Error)]
pub enum BuiltinReleaseError {
    /// Matrix JSON or filesystem input is invalid.
    #[error("invalid built-in release input `{value}`; expected {expected}")]
    InvalidInput {
        /// Offending serialized value or path.
        value: String,
        /// Required shape.
        expected: String,
    },
    /// Filesystem access failed.
    #[error("built-in release filesystem operation failed for `{path}`: {source}")]
    Io {
        /// Affected path.
        path: std::path::PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Generic signed package construction failed.
    #[error(transparent)]
    Package(#[from] lumvise_plugin_package::PackageError),
    /// Deterministic metadata serialization failed.
    #[error("built-in release metadata serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Cargo failed to compile the four built-in executables for one target.
    #[error("built-in plugin compilation failed for target `{target}` with status `{status}`")]
    Compilation {
        /// Requested Rust target triple.
        target: String,
        /// Cargo exit status or process-start failure.
        status: String,
    },
}

/// Result type for built-in release orchestration.
pub type Result<T> = std::result::Result<T, BuiltinReleaseError>;
