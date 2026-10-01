use super::*;

#[test]
fn archive_above_compressed_size_limit_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let archive_size = std::fs::metadata(&package_path)
        .expect("package metadata")
        .len();
    let limits = VerificationLimits {
        max_archive_bytes: archive_size - 1,
        ..VerificationLimits::default()
    };

    let error = verify_package_with_limits(
        &package_path,
        &fixture.public_key(),
        &compatible_host(),
        &limits,
    )
    .err()
    .expect("oversized archive rejected");

    assert!(matches!(
        error,
        PackageError::ArchiveTooLarge { actual, max }
            if actual == archive_size && max == archive_size - 1
    ));
}

#[test]
fn metadata_entry_above_uncompressed_entry_limit_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let manifest_size = serde_json::to_vec(&fixture.manifest)
        .expect("manifest size")
        .len() as u64;
    let limits = VerificationLimits {
        max_entry_bytes: manifest_size - 1,
        ..VerificationLimits::default()
    };

    let error = verify_package_with_limits(
        &package_path,
        &fixture.public_key(),
        &compatible_host(),
        &limits,
    )
    .err()
    .expect("oversized metadata rejected");

    assert!(matches!(
        error,
        PackageError::EntryTooLarge { path, actual, max }
            if path == "manifest.json" && actual == manifest_size && max == manifest_size - 1
    ));
}

#[test]
fn total_uncompressed_tree_above_limit_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let manifest_size = serde_json::to_vec(&fixture.manifest)
        .expect("manifest size")
        .len() as u64;
    let tree_size = manifest_size + 64 + fixture.executable.len() as u64;
    let limits = VerificationLimits {
        max_total_uncompressed_bytes: tree_size - 1,
        ..VerificationLimits::default()
    };

    let error = verify_package_with_limits(
        &package_path,
        &fixture.public_key(),
        &compatible_host(),
        &limits,
    )
    .err()
    .expect("oversized uncompressed tree rejected");

    assert!(matches!(
        error,
        PackageError::UncompressedPackageTooLarge { actual, max }
            if actual == tree_size && max == tree_size - 1
    ));
}

#[test]
fn payload_that_does_not_match_signed_hash_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write_with(b"tampered executable", &[]);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("hash mismatch rejected");

    assert!(matches!(error, PackageError::HashMismatch { .. }));
}

#[test]
fn path_traversal_entry_is_rejected_before_extraction() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write_with(&fixture.executable, &[("../escape", b"owned")]);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("path traversal rejected");

    assert!(matches!(error, PackageError::UnsafePath(path) if path == "../escape"));
}

#[test]
fn symlink_entry_is_rejected_before_extraction() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write_with_symlink();

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("symlink rejected");

    assert!(matches!(error, PackageError::Symlink(path) if path == "bin/escape-link"));
}

#[test]
fn archive_with_duplicate_entry_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write_with_duplicate_executable();
    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("duplicate rejected");

    assert!(matches!(error, PackageError::DuplicatePath(_)));
}

#[test]
fn package_without_host_target_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let unsupported_host = HostCompatibility::new(3, "x86_64-unknown-linux-gnu");

    let error = verify_package(&package_path, &fixture.public_key(), &unsupported_host)
        .err()
        .expect("unsupported target rejected");

    assert!(matches!(error, PackageError::UnsupportedTarget { .. }));
}

#[test]
fn package_outside_host_protocol_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let unsupported_host = HostCompatibility::new(5, "aarch64-apple-darwin");

    let error = verify_package(&package_path, &fixture.public_key(), &unsupported_host)
        .err()
        .expect("unsupported protocol rejected");

    assert!(matches!(
        error,
        PackageError::UnsupportedProtocol { host: 5, .. }
    ));
}

#[test]
fn valid_signed_package_installs_selected_target_immutably() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let verified = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .expect("valid signed package");
    let install_root = tempfile::tempdir().expect("install root");

    let installed =
        install_verified_package(&verified, install_root.path()).expect("install package");

    assert_eq!(installed.plugin_id(), "knowledge");
    assert_eq!(installed.plugin_version(), "1.2.3");
    assert_eq!(verified.exports()[0].id, "knowledge.search");
    assert_eq!(installed.host_capabilities()[0].id, "semantic.read");
    assert_eq!(
        std::fs::read(installed.executable()).expect("installed executable"),
        fixture.executable
    );
    assert!(is_read_only(installed.executable()));
}
