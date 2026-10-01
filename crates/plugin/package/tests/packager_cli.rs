use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility, PluginManifest,
    ProtocolRange, PublisherIdentity, verify_package,
};

fn manifest() -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "cli.release.1".into(),
        },
        plugin_id: "cli.plugin".into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange { min: 1, max: 1 },
        targets: BTreeMap::from([("test-target".into(), "bin/plugin".into())]),
        files: BTreeMap::from([("bin/plugin".into(), String::new())]),
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

#[test]
fn cli_builds_source_free_package_from_protected_key_file() {
    let workspace = tempfile::tempdir().expect("workspace");
    let payload = workspace.path().join("payload/bin");
    std::fs::create_dir_all(&payload).expect("payload");
    std::fs::write(payload.join("plugin"), b"compiled").expect("binary");
    let manifest_path = workspace.path().join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest()).expect("manifest"),
    )
    .expect("manifest file");
    let key = SigningKey::from_bytes(&[61; 32]);
    let key_path = workspace.path().join("release.key");
    write_private_key(&key_path, &key.to_bytes());
    let output = workspace.path().join("plugin.lvp");

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_lumvise-plugin-packager"))
        .args([
            manifest_path.as_os_str(),
            workspace.path().join("payload").as_os_str(),
            key_path.as_os_str(),
            output.as_os_str(),
        ])
        .status()
        .expect("packager process");

    assert!(status.success());
    verify_package(
        &output,
        &key.verifying_key(),
        &HostCompatibility::new(1, "test-target"),
    )
    .expect("CLI package verifies");
}

#[cfg(unix)]
#[test]
fn cli_rejects_group_readable_signing_key_without_publishing() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempfile::tempdir().expect("workspace");
    let payload = workspace.path().join("payload/bin");
    std::fs::create_dir_all(&payload).expect("payload");
    std::fs::write(payload.join("plugin"), b"compiled").expect("binary");
    let manifest_path = workspace.path().join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest()).expect("manifest"),
    )
    .expect("manifest file");
    let key_path = workspace.path().join("release.key");
    std::fs::write(&key_path, [62; 32]).expect("key file");
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o640))
        .expect("insecure key permissions");
    let output = workspace.path().join("plugin.lvp");
    let payload_root = workspace.path().join("payload");

    let result = std::process::Command::new(env!("CARGO_BIN_EXE_lumvise-plugin-packager"))
        .args([&manifest_path, &payload_root, &key_path, &output])
        .output()
        .expect("packager process");

    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("expected no group/other permissions")
    );
}

#[cfg(unix)]
fn write_private_key(path: &std::path::Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, bytes).expect("key file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("private key permissions");
}

#[cfg(not(unix))]
fn write_private_key(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("key file");
}
