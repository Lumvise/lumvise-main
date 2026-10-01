use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility, PackageError,
    PluginManifest, ProtocolRange, PublisherIdentity, build_package_from_directory, verify_package,
};

fn manifest() -> PluginManifest {
    PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "directory.release.1".into(),
        },
        plugin_id: "directory.plugin".into(),
        plugin_version: "1.0.0".into(),
        protocol: ProtocolRange { min: 1, max: 1 },
        targets: BTreeMap::from([("test-target".into(), "bin/plugin".into())]),
        files: BTreeMap::from([
            ("assets/view.html".into(), "placeholder".into()),
            ("bin/plugin".into(), "placeholder".into()),
        ]),
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

fn payload(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("assets")).expect("assets directory");
    std::fs::create_dir_all(root.join("bin")).expect("binary directory");
    std::fs::write(root.join("assets/view.html"), b"<main>signed</main>").expect("view");
    std::fs::write(root.join("bin/plugin"), b"compiled-plugin").expect("binary");
}

#[test]
fn directory_build_computes_hashes_and_produces_verifiable_package() {
    let workspace = tempfile::tempdir().expect("workspace");
    let root = workspace.path().join("payload");
    payload(&root);
    let output = workspace.path().join("plugin.lvp");
    let key = SigningKey::from_bytes(&[51; 32]);

    build_package_from_directory(&output, manifest(), &root, &key).expect("directory package");

    verify_package(
        &output,
        &key.verifying_key(),
        &HostCompatibility::new(1, "test-target"),
    )
    .expect("verified directory package");
}

#[test]
fn directory_build_rejects_extra_and_missing_payloads() {
    let workspace = tempfile::tempdir().expect("workspace");
    let root = workspace.path().join("payload");
    payload(&root);
    std::fs::write(root.join("unsigned.txt"), b"extra").expect("extra payload");
    let key = SigningKey::from_bytes(&[52; 32]);

    let extra =
        build_package_from_directory(&workspace.path().join("extra.lvp"), manifest(), &root, &key)
            .expect_err("extra payload rejected");
    std::fs::remove_file(root.join("unsigned.txt")).expect("remove extra");
    std::fs::remove_file(root.join("assets/view.html")).expect("remove expected");
    let missing = build_package_from_directory(
        &workspace.path().join("missing.lvp"),
        manifest(),
        &root,
        &key,
    )
    .expect_err("missing payload rejected");

    assert!(matches!(extra, PackageError::UnsignedFile(path) if path == "unsigned.txt"));
    assert!(matches!(missing, PackageError::MissingFile(path) if path == "assets/view.html"));
}

#[cfg(unix)]
#[test]
fn directory_build_rejects_symlinked_payload() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("workspace");
    let root = workspace.path().join("payload");
    payload(&root);
    std::fs::remove_file(root.join("bin/plugin")).expect("remove binary");
    symlink(root.join("assets/view.html"), root.join("bin/plugin")).expect("payload symlink");

    let error = build_package_from_directory(
        &workspace.path().join("symlink.lvp"),
        manifest(),
        &root,
        &SigningKey::from_bytes(&[53; 32]),
    )
    .expect_err("symlink rejected");

    assert!(matches!(error, PackageError::Symlink(path) if path == "bin/plugin"));
}
