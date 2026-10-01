use super::*;

#[test]
fn package_signed_by_untrusted_key_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let package_path = fixture.write();
    let untrusted_key = SigningKey::from_bytes(&[9; 32]).verifying_key();

    let error = verify_package(&package_path, &untrusted_key, &compatible_host())
        .err()
        .expect("untrusted signature rejected");

    assert!(matches!(error, PackageError::SignatureVerification));
}

#[test]
fn duplicate_export_identity_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    manifest.exports.push(manifest.exports[0].clone());
    let package_path = fixture.write_manifest(&manifest);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("duplicate export rejected");

    assert!(matches!(error, PackageError::DuplicateExportId(id) if id == "knowledge.search"));
}

#[test]
fn empty_export_name_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    manifest.exports[0].name = "   ".into();
    let package_path = fixture.write_manifest(&manifest);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("empty export name rejected");

    assert!(matches!(error, PackageError::InvalidExportName { .. }));
}

#[test]
fn non_object_export_schema_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    manifest.exports[0].input_schema = serde_json::json!("not-a-schema-object");
    let package_path = fixture.write_manifest(&manifest);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("invalid schema rejected");

    assert!(matches!(
        error,
        PackageError::InvalidExportSchema {
            field: "input_schema",
            ..
        }
    ));
}

#[test]
fn invalid_export_execution_shape_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = serde_json::to_value(&fixture.manifest).expect("manifest value");
    manifest["exports"][0]["execution"] = serde_json::json!("immediate");
    let manifest = serde_json::to_vec(&manifest).expect("invalid execution manifest");
    let package_path = fixture.write_raw(&manifest, &manifest, &fixture.executable, &[]);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("invalid execution rejected");

    assert!(matches!(error, PackageError::InvalidManifest(_)));
}

#[test]
fn unsigned_export_mutation_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    manifest.exports[0].name = "Mutated export".into();
    let package_path = fixture.write_unsigned_manifest_mutation(&manifest);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("unsigned export mutation rejected");

    assert!(matches!(error, PackageError::SignatureVerification));
}

#[test]
fn duplicate_host_capability_identity_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    manifest
        .host_capabilities
        .push(manifest.host_capabilities[0].clone());
    let package_path = fixture.write_manifest(&manifest);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("duplicate Host Capability rejected");

    assert!(matches!(error, PackageError::DuplicateHostCapabilityId(_)));
}

#[test]
fn invalid_host_capability_version_is_rejected() {
    let fixture = SignedPackageFixture::valid();
    let mut manifest = fixture.manifest.clone();
    manifest.host_capabilities[0].version = "version one".into();
    let package_path = fixture.write_manifest(&manifest);

    let error = verify_package(&package_path, &fixture.public_key(), &compatible_host())
        .err()
        .expect("invalid Host Capability version rejected");

    assert!(matches!(
        error,
        PackageError::InvalidHostCapabilityVersion { .. }
    ));
}
