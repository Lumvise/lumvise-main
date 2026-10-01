use std::{collections::BTreeMap, fs::File};

use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility,
    PackageError, PluginManifest, ProtocolRange, PublisherIdentity, build_package,
    install_verified_package, verify_package,
};
use sha2::{Digest, Sha256};
use zip::{DateTime, ZipArchive};

struct DeterministicPackageFixture {
    signing_key: SigningKey,
    manifest: PluginManifest,
    files: BTreeMap<String, Vec<u8>>,
}

impl DeterministicPackageFixture {
    fn valid() -> Self {
        let executable = b"#!/bin/sh\nprintf deterministic-plugin".to_vec();
        let executable_path = "bin/deterministic-plugin".to_owned();
        let files = BTreeMap::from([(executable_path.clone(), executable.clone())]);
        let manifest = PluginManifest {
            schema_version: 1,
            publisher: PublisherIdentity {
                publisher_id: "lumvise.test".into(),
                key_id: "builder.release.1".into(),
            },
            plugin_id: "deterministic.plugin".into(),
            plugin_version: "1.0.0".into(),
            protocol: ProtocolRange { min: 1, max: 1 },
            targets: BTreeMap::from([("test-target".into(), executable_path.clone())]),
            files: BTreeMap::from([(executable_path, hex::encode(Sha256::digest(&executable)))]),
            exports: vec![ExportDescriptor {
                id: "deterministic.manifest".into(),
                name: "Deterministic manifest".into(),
                description: String::new(),
                surface: ExportSurface::Command,
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: serde_json::json!({"type": "object"}),
                admission: None,
                execution: ExecutionMode::Foreground,
            }],
            host_capabilities: Vec::new(),
        };
        Self {
            signing_key: SigningKey::from_bytes(&[19; 32]),
            manifest,
            files,
        }
    }

    fn request(&self) -> BuildPackageRequest<'_> {
        BuildPackageRequest {
            manifest: self.manifest.clone(),
            files: self.files.clone(),
            signing_key: &self.signing_key,
        }
    }
}

#[test]
fn built_package_verifies_and_installs_through_public_api() {
    let fixture = DeterministicPackageFixture::valid();
    let workspace = tempfile::tempdir().expect("workspace");
    let package_path = workspace.path().join("deterministic.lvp");
    build_package(&package_path, fixture.request()).expect("build signed package");

    let verified = verify_package(
        &package_path,
        &fixture.signing_key.verifying_key(),
        &HostCompatibility::new(1, "test-target"),
    )
    .expect("verify built package");
    let installed = install_verified_package(&verified, &workspace.path().join("installed"))
        .expect("install built package");

    assert_eq!(installed.plugin_id(), "deterministic.plugin");
    assert_eq!(
        std::fs::read(installed.executable()).expect("installed executable"),
        fixture.files["bin/deterministic-plugin"]
    );
}

#[test]
fn identical_request_produces_byte_for_byte_identical_packages() {
    let fixture = DeterministicPackageFixture::valid();
    let workspace = tempfile::tempdir().expect("workspace");
    let first = workspace.path().join("first.lvp");
    let second = workspace.path().join("second.lvp");

    build_package(&first, fixture.request()).expect("first deterministic build");
    build_package(&second, fixture.request()).expect("second deterministic build");

    assert_eq!(
        std::fs::read(&first).expect("first bytes"),
        std::fs::read(&second).expect("second bytes")
    );
    let mut archive = ZipArchive::new(File::open(&second).expect("open deterministic package"))
        .expect("read deterministic ZIP");
    let metadata = (0..archive.len())
        .map(|index| {
            let entry = archive.by_index(index).expect("deterministic entry");
            (
                entry.name().to_owned(),
                entry.last_modified().unwrap_or_default(),
                entry.unix_mode().expect("fixed Unix permissions") & 0o777,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        metadata,
        vec![
            (
                "bin/deterministic-plugin".into(),
                DateTime::default(),
                0o755
            ),
            ("manifest.json".into(), DateTime::default(), 0o644),
            ("signature.ed25519".into(), DateTime::default(), 0o644),
        ]
    );
}

#[test]
fn mutating_built_payload_is_rejected() {
    let fixture = DeterministicPackageFixture::valid();
    let workspace = tempfile::tempdir().expect("workspace");
    let package_path = workspace.path().join("mutated.lvp");
    build_package(&package_path, fixture.request()).expect("build signed package");
    let mut package = std::fs::read(&package_path).expect("package bytes");
    let payload = &fixture.files["bin/deterministic-plugin"];
    let offset = package
        .windows(payload.len())
        .position(|window| window == payload)
        .expect("stored executable payload");
    package[offset] ^= 0xff;
    std::fs::write(&package_path, package).expect("mutate package");

    let error = verify_package(
        &package_path,
        &fixture.signing_key.verifying_key(),
        &HostCompatibility::new(1, "test-target"),
    )
    .err()
    .expect("mutated package rejected");

    assert!(matches!(error, PackageError::InvalidArchive(_)));
}

#[test]
fn invalid_build_preserves_existing_package_and_leaves_no_temporary_file() {
    let mut fixture = DeterministicPackageFixture::valid();
    fixture
        .manifest
        .files
        .insert("bin/deterministic-plugin".into(), "0".repeat(64));
    let workspace = tempfile::tempdir().expect("workspace");
    let package_path = workspace.path().join("existing.lvp");
    std::fs::write(&package_path, b"existing-package-sentinel").expect("write sentinel");

    let error =
        build_package(&package_path, fixture.request()).expect_err("invalid build rejected");

    assert!(matches!(error, PackageError::HashMismatch { .. }));
    assert_eq!(
        std::fs::read(&package_path).expect("preserved package"),
        b"existing-package-sentinel"
    );
    assert_eq!(
        std::fs::read_dir(workspace.path())
            .expect("workspace entries")
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn atomic_replace_publishes_over_read_only_existing_package() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = DeterministicPackageFixture::valid();
    let workspace = tempfile::tempdir().expect("workspace");
    let package_path = workspace.path().join("replace.lvp");
    std::fs::write(&package_path, b"read-only-sentinel").expect("write sentinel");
    std::fs::set_permissions(&package_path, std::fs::Permissions::from_mode(0o444))
        .expect("make sentinel read-only");

    build_package(&package_path, fixture.request()).expect("atomic package replacement");

    let verified = verify_package(
        &package_path,
        &fixture.signing_key.verifying_key(),
        &HostCompatibility::new(1, "test-target"),
    );
    assert!(verified.is_ok());
}
