use std::{collections::BTreeMap, path::PathBuf};

use lumvise_plugin_package::PluginManifest;
use serde::{Deserialize, Serialize};

use crate::{BuiltinReleaseDescriptor, BuiltinReleaseError, Result};

/// Plugin identity to its compiled source-independent executable path.
pub type BuiltinBinaryPaths = BTreeMap<String, PathBuf>;

/// Descriptor-driven target matrix used to create signed Built-in Plugin packages.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinReleaseMatrix {
    /// Plugin identity to crate-owned release descriptor.
    pub descriptors: BTreeMap<String, BuiltinReleaseDescriptor>,
    /// Plugin identity to declarative package manifest template.
    pub manifests: BTreeMap<String, PluginManifest>,
    /// Target triple to compiled executable paths keyed by plugin identity.
    pub targets: BTreeMap<String, BuiltinBinaryPaths>,
}

impl BuiltinReleaseMatrix {
    /// Reads and validates a release matrix JSON file.
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|source| BuiltinReleaseError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let matrix: Self = serde_json::from_slice(&bytes)?;
        matrix.validate()?;
        Ok(matrix)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.targets.is_empty() || self.descriptors.is_empty() {
            return Err(invalid(
                "matrix",
                "at least one target and Built-in Plugin descriptor",
            ));
        }
        if self.descriptors.keys().ne(self.manifests.keys()) {
            return Err(invalid("manifests", "exactly one manifest per descriptor"));
        }
        for (plugin_id, descriptor) in &self.descriptors {
            validate_manifest(plugin_id, descriptor, &self.manifests[plugin_id])?;
        }
        for (target, binaries) in &self.targets {
            validate_target(target)?;
            validate_binaries(target, binaries, &self.descriptors)?;
        }
        Ok(())
    }
}

fn validate_manifest(
    plugin_id: &str,
    descriptor: &BuiltinReleaseDescriptor,
    manifest: &PluginManifest,
) -> Result<()> {
    let identity_matches = descriptor.plugin_id == plugin_id
        && manifest.plugin_id == plugin_id
        && manifest.plugin_version == descriptor.plugin_version
        && manifest.publisher.publisher_id == descriptor.publisher_id
        && manifest.publisher.key_id == descriptor.key_id
        && manifest.protocol.min == descriptor.protocol_min
        && manifest.protocol.max == descriptor.protocol_max;
    if identity_matches && manifest.targets.is_empty() && manifest.files.is_empty() {
        return Ok(());
    }
    Err(invalid(
        plugin_id,
        "descriptor-matched manifest template with empty target/file maps",
    ))
}

fn validate_target(target: &str) -> Result<()> {
    if !target.trim().is_empty() && !target.chars().any(char::is_whitespace) {
        return Ok(());
    }
    Err(invalid(
        target,
        "non-empty target triple without whitespace",
    ))
}

fn validate_binaries(
    target: &str,
    binaries: &BuiltinBinaryPaths,
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> Result<()> {
    if binaries.keys().ne(descriptors.keys()) {
        return Err(invalid(target, "one compiled executable per descriptor"));
    }
    for (plugin_id, path) in binaries {
        if !path.is_file() {
            return Err(invalid(
                &path.display().to_string(),
                &format!("compiled `{plugin_id}` executable for target `{target}`"),
            ));
        }
    }
    Ok(())
}

fn invalid(value: &str, expected: &str) -> BuiltinReleaseError {
    BuiltinReleaseError::InvalidInput {
        value: value.into(),
        expected: expected.into(),
    }
}
