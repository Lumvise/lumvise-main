use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use lumvise_plugin_knowledge::{PACKAGE_PROTOCOL_VERSION, package_manifest_source};
use lumvise_plugin_package::{
    BuildPackageRequest, HostCompatibility, build_package, verify_package,
};
use sha2::{Digest, Sha256};

#[test]
fn signed_knowledge_archive_rejects_post_signing_tamper() {
    const TARGET: &str = "knowledge-tamper-host";
    let workspace = tempfile::tempdir().expect("tamper workspace");
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
        .expect("read Knowledge binary");
    let executable_hash = hex::encode(Sha256::digest(&executable));
    let manifest = package_manifest_source(TARGET, &executable_hash);
    let executable_path = manifest.targets[TARGET].clone();
    let signing_key = SigningKey::from_bytes(&[31; 32]);
    let archive = workspace.path().join("tampered.lvp");
    build_package(
        &archive,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key: &signing_key,
        },
    )
    .expect("build signed package");
    let mut bytes = std::fs::read(&archive).expect("read package archive");
    let offset = bytes.len() / 2;
    bytes[offset] ^= 0x5a;
    std::fs::write(&archive, bytes).expect("tamper package archive");

    let error = match verify_package(
        &archive,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    ) {
        Ok(_) => panic!("post-signing tamper was accepted"),
        Err(error) => error,
    };
    assert!(!error.to_string().is_empty());
}
