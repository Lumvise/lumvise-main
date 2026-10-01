use std::collections::{BTreeMap, BTreeSet};
use std::{fs, path::Path};

use ed25519_dalek::VerifyingKey;
use serde::Deserialize;

use crate::{
    HostCompatibility, PackageError, PublisherIdentity, VerificationLimits, VerifiedPackage,
    archive,
};

#[derive(Debug)]
struct TrustedKey {
    publisher_id: String,
    key: VerifyingKey,
}

/// In-memory publisher key registry and production package verification boundary.
///
/// Key IDs are globally unique. Revocation and publisher/key binding are checked
/// from canonical signed-manifest metadata before package payloads are read.
#[derive(Debug, Default)]
pub struct PublisherTrustStore {
    keys: BTreeMap<String, TrustedKey>,
    publishers: BTreeSet<String>,
    revoked: BTreeSet<String>,
}

impl PublisherTrustStore {
    /// Creates an empty trust store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads strict publisher trust and revocation policy from persistent JSON.
    ///
    /// # Errors
    /// Returns [`PackageError::Io`] when unreadable or
    /// [`PackageError::InvalidTrustStore`] when malformed or unsupported.
    pub fn load_json_file(path: &Path) -> Result<Self, PackageError> {
        let bytes = fs::read(path).map_err(|source| PackageError::Io {
            path: path.to_owned(),
            source,
        })?;
        let document: TrustStoreDocument = serde_json::from_slice(&bytes)
            .map_err(|error| invalid_trust_store(path, error.to_string()))?;
        if document.schema_version != 1 {
            return Err(invalid_trust_store(
                path,
                format!("schema_version `{}`; expected `1`", document.schema_version),
            ));
        }
        let mut store = Self::new();
        for record in document.keys {
            let key_bytes = decode_verifying_key(path, &record)?;
            let key = VerifyingKey::from_bytes(&key_bytes)
                .map_err(|error| invalid_trust_store(path, error.to_string()))?;
            store
                .add_key(&record.publisher_id, &record.key_id, key)
                .map_err(|error| invalid_trust_store(path, error.to_string()))?;
            if record.revoked {
                store
                    .revoke_key(&record.key_id)
                    .map_err(|error| invalid_trust_store(path, error.to_string()))?;
            }
        }
        Ok(store)
    }

    /// Adds one publisher-bound verification key.
    ///
    /// # Errors
    /// Returns [`PackageError::DuplicatePublisherKeyId`] when `key_id` exists.
    pub fn add_key(
        &mut self,
        publisher_id: impl Into<String>,
        key_id: impl Into<String>,
        key: VerifyingKey,
    ) -> Result<(), PackageError> {
        let publisher_id = publisher_id.into();
        let key_id = key_id.into();
        PublisherIdentity {
            publisher_id: publisher_id.clone(),
            key_id: key_id.clone(),
        }
        .validate()?;
        if self.keys.contains_key(&key_id) {
            return Err(PackageError::DuplicatePublisherKeyId(key_id));
        }
        self.publishers.insert(publisher_id.clone());
        self.keys.insert(key_id, TrustedKey { publisher_id, key });
        Ok(())
    }

    /// Revokes a known key without removing its publisher binding.
    ///
    /// # Errors
    /// Returns [`PackageError::UnknownPublisherKey`] when `key_id` is absent.
    pub fn revoke_key(&mut self, key_id: &str) -> Result<(), PackageError> {
        if !self.keys.contains_key(key_id) {
            return Err(PackageError::UnknownPublisherKey(key_id.to_owned()));
        }
        self.revoked.insert(key_id.to_owned());
        Ok(())
    }

    /// Verifies a package using its signed publisher/key identity and safe limits.
    ///
    /// # Errors
    /// Returns trust, compatibility, archive, hash-tree, or signature errors.
    pub fn verify_package(
        &self,
        package_path: &Path,
        host: &HostCompatibility,
    ) -> Result<VerifiedPackage, PackageError> {
        self.verify_package_with_limits(package_path, host, &VerificationLimits::default())
    }

    /// Verifies a publisher package with explicit resource limits.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::verify_package`] plus limit errors.
    pub fn verify_package_with_limits(
        &self,
        package_path: &Path,
        host: &HostCompatibility,
        limits: &VerificationLimits,
    ) -> Result<VerifiedPackage, PackageError> {
        let loaded = archive::load(package_path, limits)?;
        let manifest = archive::manifest(&loaded, limits)?;
        manifest.publisher.validate()?;
        let trusted =
            self.trusted_key(&manifest.publisher.publisher_id, &manifest.publisher.key_id)?;
        archive::verify_loaded(&loaded, &trusted.key, host, limits).map(VerifiedPackage)
    }

    fn trusted_key(&self, publisher_id: &str, key_id: &str) -> Result<&TrustedKey, PackageError> {
        if !self.publishers.contains(publisher_id) {
            return Err(PackageError::UnknownPublisher(publisher_id.to_owned()));
        }
        let trusted = self
            .keys
            .get(key_id)
            .ok_or_else(|| PackageError::UnknownPublisherKey(key_id.to_owned()))?;
        if trusted.publisher_id != publisher_id {
            return Err(PackageError::PublisherKeyMismatch {
                key_id: key_id.to_owned(),
                expected_publisher: trusted.publisher_id.clone(),
                actual_publisher: publisher_id.to_owned(),
            });
        }
        if self.revoked.contains(key_id) {
            return Err(PackageError::RevokedPublisherKey(key_id.to_owned()));
        }
        Ok(trusted)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustStoreDocument {
    schema_version: u32,
    keys: Vec<TrustKeyRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustKeyRecord {
    publisher_id: String,
    key_id: String,
    public_key_hex: String,
    #[serde(default)]
    revoked: bool,
}

fn decode_verifying_key(path: &Path, record: &TrustKeyRecord) -> Result<[u8; 32], PackageError> {
    let decoded = hex::decode(&record.public_key_hex).map_err(|error| {
        invalid_trust_store(
            path,
            format!("public key `{}` is not hex: {error}", record.public_key_hex),
        )
    })?;
    decoded.try_into().map_err(|bytes: Vec<u8>| {
        invalid_trust_store(
            path,
            format!(
                "public key `{}` has {} bytes; expected 32",
                record.key_id,
                bytes.len()
            ),
        )
    })
}

fn invalid_trust_store(path: &Path, message: String) -> PackageError {
    PackageError::InvalidTrustStore {
        path: path.to_owned(),
        message,
    }
}
