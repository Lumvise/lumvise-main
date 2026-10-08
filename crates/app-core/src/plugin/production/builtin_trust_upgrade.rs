//! Prepares only the approved built-in authority upgrade before bundled installation.
//! The startup owner calls `BuiltinTrustUpgrade::prepare`; policy records stay private.

use super::invalid_policy;
use crate::Result;
use lumvise_plugin_package::{HostCompatibility, PluginReleaseIndexV2, PublisherTrustStore};
use serde::{Deserialize, Serialize};
use std::{fs, fs::OpenOptions, io::Write, path::Path};

const BUILTIN_PUBLISHER: &str = "lumvise.builtin";
const DEVELOPMENT_KEY_ID: &str = "lumvise.release.1";
const PRODUCTION_KEY_ID: &str = "lumvise.production.1";
const DEVELOPMENT_PUBLIC_KEY: &str =
    "6b734a8eff246fe734b38d4046c148eee5f04fe87b3a0a423955a77956de066b";
const PRODUCTION_PUBLIC_KEY: &str =
    "501578fa9b8f21eabf49f4aa560b890845a07d205f5639cf324bc3f6e037967f";

#[derive(Clone, Debug)]
pub(super) struct BuiltinTrustUpgrade {
    development_public_key: String,
    production_public_key: String,
    protocol_major: u32,
}

impl BuiltinTrustUpgrade {
    pub(super) fn production() -> Self {
        Self {
            development_public_key: DEVELOPMENT_PUBLIC_KEY.into(),
            production_public_key: PRODUCTION_PUBLIC_KEY.into(),
            protocol_major: u32::from(lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION.major),
        }
    }

    #[cfg(test)]
    fn with_test_pins(development: String, production: String) -> Self {
        Self {
            development_public_key: development,
            production_public_key: production,
            protocol_major: 1,
        }
    }

    pub(super) fn prepare(
        &self,
        release: &Path,
        installed: &Path,
        index: &PluginReleaseIndexV2,
    ) -> Result<()> {
        if !installed.exists() {
            return Ok(());
        }
        let (original, mut document) = read_installed_policy(installed)?;
        let Some(legacy) = self.eligible_record(&document) else {
            return Ok(());
        };
        if !self.preflight_release(release, installed, index)? {
            return Ok(());
        }
        self.prepare_records(installed, &mut document, legacy)?;
        publish_policy(installed, &original, &document)
    }

    fn eligible_record(&self, document: &TrustUpgradeDocument) -> Option<usize> {
        document.keys.iter().position(|record| {
            record.matches(DEVELOPMENT_KEY_ID, &self.development_public_key) && !record.revoked
        })
    }

    fn preflight_release(
        &self,
        release: &Path,
        installed: &Path,
        index: &PluginReleaseIndexV2,
    ) -> Result<bool> {
        let path = release.join("publisher-trust.json");
        let bytes = fs::read(&path).map_err(|error| invalid_policy(&path, error.to_string()))?;
        let document = TrustUpgradeDocument::parse(&path, &bytes)?;
        let trust = validated_snapshot(installed, &document)?;
        if !self.require_bundle_pin(&path, &document)? {
            return Ok(false);
        }
        for artifact in &index.artifacts {
            self.verify_candidate(release, &trust, artifact)?;
        }
        Ok(true)
    }

    fn require_bundle_pin(&self, path: &Path, document: &TrustUpgradeDocument) -> Result<bool> {
        let Some(record) = document
            .keys
            .iter()
            .find(|record| record.key_id == PRODUCTION_KEY_ID)
        else {
            return Ok(false);
        };
        if !record.matches(PRODUCTION_KEY_ID, &self.production_public_key) || record.revoked {
            return Err(invalid_policy(
                path,
                format!(
                    "key {:?} publisher {:?} public key {:?} revoked {}; expected active {BUILTIN_PUBLISHER}/{PRODUCTION_KEY_ID} compiled production pin",
                    record.key_id, record.publisher_id, record.public_key_hex, record.revoked
                ),
            ));
        }
        Ok(true)
    }

    fn verify_candidate(
        &self,
        release: &Path,
        trust: &PublisherTrustStore,
        artifact: &lumvise_plugin_package::PluginReleaseArtifactV2,
    ) -> Result<()> {
        require_production_identity(release, artifact)?;
        for target in &artifact.targets {
            let host = HostCompatibility::new(self.protocol_major, target);
            trust.verify_package(&release.join(&artifact.archive_path), &host)?;
        }
        Ok(())
    }

    fn prepare_records(
        &self,
        path: &Path,
        document: &mut TrustUpgradeDocument,
        legacy: usize,
    ) -> Result<()> {
        if !self.require_bundle_pin(path, document)? {
            document.keys.push(TrustUpgradeKey {
                publisher_id: BUILTIN_PUBLISHER.into(),
                key_id: PRODUCTION_KEY_ID.into(),
                public_key_hex: self.production_public_key.clone(),
                revoked: false,
            });
        }
        document.keys[legacy].revoked = true;
        Ok(())
    }
}

fn read_installed_policy(path: &Path) -> Result<(Vec<u8>, TrustUpgradeDocument)> {
    let original = fs::read(path).map_err(|error| invalid_policy(path, error.to_string()))?;
    let document = TrustUpgradeDocument::parse(path, &original)?;
    validated_snapshot(path, &document)?;
    Ok((original, document))
}

fn require_production_identity(
    path: &Path,
    artifact: &lumvise_plugin_package::PluginReleaseArtifactV2,
) -> Result<()> {
    if artifact.publisher_id == BUILTIN_PUBLISHER && artifact.key_id == PRODUCTION_KEY_ID {
        return Ok(());
    }
    Err(invalid_policy(
        path,
        format!(
            "artifact {:?} publisher/key {:?}/{:?}; expected {BUILTIN_PUBLISHER}/{PRODUCTION_KEY_ID}",
            artifact.plugin_id, artifact.publisher_id, artifact.key_id
        ),
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TrustUpgradeDocument {
    schema_version: u32,
    keys: Vec<TrustUpgradeKey>,
}

impl TrustUpgradeDocument {
    fn parse(path: &Path, bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|error| invalid_policy(path, error.to_string()))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TrustUpgradeKey {
    publisher_id: String,
    key_id: String,
    public_key_hex: String,
    #[serde(default)]
    revoked: bool,
}

impl TrustUpgradeKey {
    fn matches(&self, key_id: &str, public_key: &str) -> bool {
        self.publisher_id == BUILTIN_PUBLISHER
            && self.key_id == key_id
            && self.public_key_hex.eq_ignore_ascii_case(public_key)
    }
}

fn publish_policy(path: &Path, original: &[u8], document: &TrustUpgradeDocument) -> Result<()> {
    let temporary = snapshot_path(path)?;
    let outcome = write_policy(&temporary, document)
        .and_then(|()| replace_snapshot(path, &temporary, original));
    let _ = fs::remove_file(temporary);
    outcome
}

fn validated_snapshot(path: &Path, document: &TrustUpgradeDocument) -> Result<PublisherTrustStore> {
    // Validate and verify one immutable snapshot so a reread cannot substitute another authority.
    let temporary = snapshot_path(path)?;
    let outcome = write_policy(&temporary, document)
        .and_then(|()| PublisherTrustStore::load_json_file(&temporary).map_err(Into::into));
    let _ = fs::remove_file(temporary);
    outcome
}

fn snapshot_path(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_policy(path, "expected policy parent directory".into()))?;
    Ok(parent.join(format!(
        ".builtin-trust-upgrade-{}.tmp",
        uuid::Uuid::new_v4()
    )))
}

fn replace_snapshot(path: &Path, temporary: &Path, original: &[u8]) -> Result<()> {
    let current = fs::read(path).map_err(|error| invalid_policy(path, error.to_string()))?;
    if current != original {
        return Err(invalid_policy(
            path,
            "installed policy changed; expected the inspected administrator policy".into(),
        ));
    }
    fs::rename(temporary, path).map_err(|error| invalid_policy(path, error.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid_policy(path, "expected policy parent directory".into()))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| invalid_policy(parent, error.to_string()))
}

fn write_policy(path: &Path, document: &TrustUpgradeDocument) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| invalid_policy(path, error.to_string()))?;
    let bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| invalid_policy(path, error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| invalid_policy(path, error.to_string()))
}

#[cfg(test)]
mod tests;
