use std::{collections::BTreeMap, fs::File, io::Write, path::PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use lumvise_plugin_package::{
    BuildPackageRequest, HostCompatibility, PackageError, PluginManifest, PublisherIdentity,
    PublisherTrustStore, build_package,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

struct TrustFixture {
    root: TempDir,
    key: SigningKey,
    manifest: PluginManifest,
    executable: Vec<u8>,
}

impl TrustFixture {
    fn new() -> Self {
        let executable = b"publisher executable".to_vec();
        let mut manifest = base_manifest(&executable);
        manifest.publisher = PublisherIdentity {
            publisher_id: "publisher.alpha".into(),
            key_id: "alpha.release.1".into(),
        };
        Self {
            root: tempfile::tempdir().expect("trust workspace"),
            key: SigningKey::from_bytes(&[41; 32]),
            manifest,
            executable,
        }
    }

    fn build(&self, manifest: PluginManifest, key: &SigningKey, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        build_package(
            &path,
            BuildPackageRequest {
                manifest,
                files: BTreeMap::from([("bin/plugin".into(), self.executable.clone())]),
                signing_key: key,
            },
        )
        .expect("build trust package");
        path
    }

    fn package(&self) -> PathBuf {
        self.build(self.manifest.clone(), &self.key, "trusted.lvp")
    }

    fn trust(&self) -> PublisherTrustStore {
        let mut trust = PublisherTrustStore::new();
        trust
            .add_key(
                "publisher.alpha",
                "alpha.release.1",
                self.key.verifying_key(),
            )
            .expect("add trusted key");
        trust
    }
}

#[test]
fn trusted_package_verifies_and_duplicate_key_id_is_rejected() {
    let fixture = TrustFixture::new();
    let mut trust = fixture.trust();
    assert!(trust.verify_package(&fixture.package(), &host()).is_ok());

    let error = trust
        .add_key(
            "publisher.bravo",
            "alpha.release.1",
            SigningKey::from_bytes(&[42; 32]).verifying_key(),
        )
        .expect_err("duplicate key rejected");
    assert!(matches!(error, PackageError::DuplicatePublisherKeyId(_)));
}

#[test]
fn persistent_trust_document_loads_key_and_revocation_policy() {
    let fixture = TrustFixture::new();
    let trust_path = fixture.root.path().join("publisher-trust.json");
    std::fs::write(
        &trust_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "keys": [{
                "publisher_id": "publisher.alpha",
                "key_id": "alpha.release.1",
                "public_key_hex": hex::encode(fixture.key.verifying_key().to_bytes()),
                "revoked": false
            }]
        }))
        .expect("serialize trust document"),
    )
    .expect("write trust document");

    let trust = PublisherTrustStore::load_json_file(&trust_path).expect("load persistent trust");
    assert!(trust.verify_package(&fixture.package(), &host()).is_ok());

    let revoked_path = fixture.root.path().join("revoked-trust.json");
    std::fs::write(
        &revoked_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "keys": [{
                "publisher_id": "publisher.alpha",
                "key_id": "alpha.release.1",
                "public_key_hex": hex::encode(fixture.key.verifying_key().to_bytes()),
                "revoked": true
            }]
        }))
        .expect("serialize revoked trust document"),
    )
    .expect("write revoked trust document");
    let revoked = PublisherTrustStore::load_json_file(&revoked_path).expect("load revocation");
    assert!(matches!(
        revoked.verify_package(&fixture.package(), &host()),
        Err(PackageError::RevokedPublisherKey(_))
    ));
}

#[test]
fn persistent_trust_document_rejects_malformed_or_unknown_fields() {
    let root = tempfile::tempdir().expect("trust workspace");
    let path = root.path().join("publisher-trust.json");
    std::fs::write(
        &path,
        br#"{"schema_version":1,"keys":[],"unexpected":true}"#,
    )
    .expect("write malformed trust document");

    let error = PublisherTrustStore::load_json_file(&path).expect_err("unknown field rejected");
    assert!(matches!(error, PackageError::InvalidTrustStore { .. }));
}

#[test]
fn unknown_publisher_and_unknown_key_are_rejected() {
    let fixture = TrustFixture::new();
    let error = PublisherTrustStore::new()
        .verify_package(&fixture.package(), &host())
        .err()
        .expect("unknown publisher");
    assert!(matches!(error, PackageError::UnknownPublisher(_)));

    let mut manifest = fixture.manifest.clone();
    manifest.publisher.key_id = "alpha.release.2".into();
    let error = fixture
        .trust()
        .verify_package(
            &fixture.build(manifest, &fixture.key, "unknown-key.lvp"),
            &host(),
        )
        .err()
        .expect("unknown key");
    assert!(matches!(error, PackageError::UnknownPublisherKey(_)));
}

#[test]
fn revoked_and_substituted_keys_are_rejected() {
    let fixture = TrustFixture::new();
    let mut trust = fixture.trust();
    trust.revoke_key("alpha.release.1").expect("revoke key");
    assert!(matches!(
        trust.verify_package(&fixture.package(), &host()),
        Err(PackageError::RevokedPublisherKey(_))
    ));

    let substituted = SigningKey::from_bytes(&[43; 32]);
    let path = fixture.build(fixture.manifest.clone(), &substituted, "substituted.lvp");
    assert!(matches!(
        fixture.trust().verify_package(&path, &host()),
        Err(PackageError::SignatureVerification)
    ));
}

#[test]
fn publisher_manifest_mutation_is_rejected_by_key_binding() {
    let fixture = TrustFixture::new();
    let signed = fixture.manifest.clone();
    let mut mutated = signed.clone();
    mutated.publisher.publisher_id = "publisher.bravo".into();
    let path = write_mutated_package(&fixture, &mutated, &signed);

    let mut trust = fixture.trust();
    trust
        .add_key(
            "publisher.bravo",
            "bravo.release.1",
            SigningKey::from_bytes(&[44; 32]).verifying_key(),
        )
        .expect("known mutated publisher");
    let error = trust
        .verify_package(&path, &host())
        .err()
        .expect("publisher mutation rejected");

    assert!(matches!(error, PackageError::PublisherKeyMismatch { .. }));
}

fn base_manifest(executable: &[u8]) -> PluginManifest {
    use lumvise_plugin_package::{ExecutionMode, ExportDescriptor, ExportSurface, ProtocolRange};
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "placeholder".into(),
            key_id: "placeholder".into(),
        },
        plugin_id: "publisher.plugin".into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange { min: 1, max: 1 },
        targets: BTreeMap::from([("test-target".into(), "bin/plugin".into())]),
        files: BTreeMap::from([("bin/plugin".into(), hex::encode(Sha256::digest(executable)))]),
        exports: vec![ExportDescriptor {
            id: "manifest".into(),
            name: "Manifest".into(),
            description: String::new(),
            surface: ExportSurface::Command,
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: serde_json::json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        }],
        host_capabilities: Vec::new(),
    }
}

fn write_mutated_package(
    fixture: &TrustFixture,
    archive_manifest: &PluginManifest,
    signed_manifest: &PluginManifest,
) -> PathBuf {
    let archive_bytes = serde_json::to_vec(archive_manifest).expect("archive manifest");
    let signed_bytes = serde_json::to_vec(signed_manifest).expect("signed manifest");
    let signature = fixture.key.sign(&signature_payload(&signed_bytes));
    let path = fixture.root.path().join("mutated.lvp");
    let mut archive = ZipWriter::new(File::create(&path).expect("mutated archive"));
    write_entry(&mut archive, "manifest.json", &archive_bytes);
    write_entry(&mut archive, "signature.ed25519", &signature.to_bytes());
    write_entry(&mut archive, "bin/plugin", &fixture.executable);
    archive.finish().expect("finish mutated archive");
    path
}

fn write_entry(archive: &mut ZipWriter<File>, path: &str, bytes: &[u8]) {
    archive
        .start_file(path, SimpleFileOptions::default())
        .expect("start entry");
    archive.write_all(bytes).expect("write entry");
}

fn signature_payload(manifest: &[u8]) -> Vec<u8> {
    [b"LUMVISE_PLUGIN_PACKAGE_V1\0".as_slice(), manifest].concat()
}

fn host() -> HostCompatibility {
    HostCompatibility::new(1, "test-target")
}
