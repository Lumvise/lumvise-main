use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{PluginManifest, VerificationLimits, archive};

/// Schema generation for immutable Lumvise composition indexes.
pub const PLUGIN_RELEASE_INDEX_SCHEMA_VERSION: u32 = 2;

/// `Full` contains every discovered Built-in Plugin; `Minimal` contains the
/// explicitly selected shipping baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseComposition {
    /// Every Built-in Plugin discovered by release tooling.
    Full,
    /// The explicit Semantic-and-Knowledge shipping baseline.
    Minimal,
}

/// One immutable signed archive in a release composition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginReleaseArtifactV2 {
    /// Stable Plugin Protocol identity.
    pub plugin_id: String,
    /// SemVer package version.
    pub plugin_version: String,
    /// Publisher identity asserted by the signed package.
    pub publisher_id: String,
    /// Signing key identity asserted by the signed package.
    pub key_id: String,
    /// Relative archive path beneath the release root.
    pub archive_path: String,
    /// Lowercase hexadecimal SHA-256 digest of the complete archive.
    pub archive_sha256: String,
    /// Sorted host target triples containing this exact archive.
    pub targets: Vec<String>,
}

/// Deterministic schema-two release index consumed by App Core bootstrap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginReleaseIndexV2 {
    /// Release-index schema generation.
    pub schema_version: u32,
    /// Product identity. Schema two accepts only `lumvise`.
    pub product: String,
    /// Built-in package membership selected for this distribution.
    pub composition: ReleaseComposition,
    /// Package archives sorted by Plugin Protocol identity.
    pub artifacts: Vec<PluginReleaseArtifactV2>,
}

/// Release-index parsing, metadata, or artifact verification failure.
#[derive(Debug, thiserror::Error)]
pub enum PluginReleaseIndexError {
    /// Index or archive filesystem access failed.
    #[error("plugin release filesystem operation failed for `{path}`: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// JSON did not match the strict schema-two contract.
    #[error("plugin release index JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    /// Metadata did not satisfy the composition contract.
    #[error("invalid plugin release value `{value}`; expected {expected}")]
    Invalid {
        /// Offending value.
        value: String,
        /// Required shape.
        expected: String,
    },
    /// Archive bytes did not match immutable release metadata.
    #[error("plugin release archive `{path}` has SHA-256 `{actual}`; expected `{expected}`")]
    Digest {
        /// Affected archive.
        path: PathBuf,
        /// Observed digest.
        actual: String,
        /// Indexed digest.
        expected: String,
    },
}

impl PluginReleaseIndexV2 {
    /// Parses and validates strict schema-two metadata.
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginReleaseIndexError> {
        let index: Self = serde_json::from_slice(bytes)?;
        index.validate_metadata()?;
        Ok(index)
    }

    /// Reads and validates one schema-two index.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, PluginReleaseIndexError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| PluginReleaseIndexError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&bytes)
    }

    /// Verifies every indexed archive and its signed manifest metadata.
    pub fn verify_artifacts(&self, root: &Path) -> Result<(), PluginReleaseIndexError> {
        for artifact in &self.artifacts {
            let path = root.join(&artifact.archive_path);
            let loaded = archive::load(&path, &VerificationLimits::default()).map_err(|error| {
                PluginReleaseIndexError::Invalid {
                    value: path.display().to_string(),
                    expected: format!("valid plugin archive: {error}"),
                }
            })?;
            let actual = format!("{:x}", Sha256::digest(loaded.bytes()));
            if actual != artifact.archive_sha256 {
                return Err(PluginReleaseIndexError::Digest {
                    path,
                    actual,
                    expected: artifact.archive_sha256.clone(),
                });
            }
            let manifest =
                archive::manifest(&loaded, &VerificationLimits::default()).map_err(|error| {
                    PluginReleaseIndexError::Invalid {
                        value: artifact.archive_path.clone(),
                        expected: format!("valid signed package manifest: {error}"),
                    }
                })?;
            verify_manifest_metadata(artifact, &manifest)?;
        }
        Ok(())
    }

    fn validate_metadata(&self) -> Result<(), PluginReleaseIndexError> {
        require(
            self.schema_version == PLUGIN_RELEASE_INDEX_SCHEMA_VERSION,
            self.schema_version.to_string(),
            format!("schema version {PLUGIN_RELEASE_INDEX_SCHEMA_VERSION}"),
        )?;
        require(
            self.product == "lumvise",
            &self.product,
            "product `lumvise`",
        )?;
        validate_artifacts(&self.artifacts)?;
        validate_composition(self.composition, &self.artifacts)
    }
}

fn verify_manifest_metadata(
    artifact: &PluginReleaseArtifactV2,
    manifest: &PluginManifest,
) -> Result<(), PluginReleaseIndexError> {
    let indexed_identity = (
        artifact.plugin_id.as_str(),
        artifact.plugin_version.as_str(),
        artifact.publisher_id.as_str(),
        artifact.key_id.as_str(),
    );
    let signed_identity = (
        manifest.plugin_id.as_str(),
        manifest.plugin_version.as_str(),
        manifest.publisher.publisher_id.as_str(),
        manifest.publisher.key_id.as_str(),
    );
    require(
        indexed_identity == signed_identity,
        format!("{indexed_identity:?}"),
        format!("signed package identity {signed_identity:?}"),
    )?;
    let signed_targets = manifest.targets.keys().cloned().collect::<Vec<_>>();
    require(
        artifact.targets == signed_targets,
        artifact.targets.join(","),
        format!("signed package targets {}", signed_targets.join(",")),
    )
}

fn validate_artifacts(
    artifacts: &[PluginReleaseArtifactV2],
) -> Result<(), PluginReleaseIndexError> {
    let mut identities = BTreeSet::new();
    for artifact in artifacts {
        let identity = format!("{}@{}", artifact.plugin_id, artifact.plugin_version);
        require(
            !artifact.plugin_id.trim().is_empty()
                && !artifact.plugin_version.trim().is_empty()
                && !artifact.publisher_id.trim().is_empty()
                && !artifact.key_id.trim().is_empty(),
            &identity,
            "non-empty package and publisher identities",
        )?;
        require(
            is_sha256(&artifact.archive_sha256),
            &artifact.archive_sha256,
            "lowercase 64-character SHA-256",
        )?;
        require(
            is_safe_relative_path(&artifact.archive_path),
            &artifact.archive_path,
            "single relative archive filename",
        )?;
        require(
            !artifact.targets.is_empty()
                && artifact.targets.windows(2).all(|pair| pair[0] < pair[1]),
            artifact.targets.join(","),
            "non-empty sorted unique target list",
        )?;
        require(
            identities.insert(identity.clone()),
            identity,
            "unique plugin identity and version",
        )?;
    }
    require(
        artifacts
            .windows(2)
            .all(|pair| pair[0].plugin_id < pair[1].plugin_id),
        "artifact order",
        "strict plugin-id order",
    )
}

fn validate_composition(
    composition: ReleaseComposition,
    artifacts: &[PluginReleaseArtifactV2],
) -> Result<(), PluginReleaseIndexError> {
    let identities = artifacts
        .iter()
        .map(|artifact| artifact.plugin_id.as_str())
        .collect::<Vec<_>>();
    match composition {
        ReleaseComposition::Minimal => require(
            identities == ["builtin.knowledge", "builtin.semantic"],
            identities.join(","),
            "exact minimal membership: builtin.knowledge,builtin.semantic",
        ),
        ReleaseComposition::Full => require(
            identities.contains(&"builtin.knowledge") && identities.contains(&"builtin.semantic"),
            identities.join(","),
            "full membership containing Semantic and Knowledge",
        ),
    }
}

fn require(
    valid: bool,
    value: impl Into<String>,
    expected: impl Into<String>,
) -> Result<(), PluginReleaseIndexError> {
    if valid {
        return Ok(());
    }
    Err(PluginReleaseIndexError::Invalid {
        value: value.into(),
        expected: expected.into(),
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}
