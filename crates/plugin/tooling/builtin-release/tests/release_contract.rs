use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ed25519_dalek::SigningKey;
use lumvise_builtin_plugin_release::{
    BuiltinBinaryPaths, BuiltinReleaseDescriptor, BuiltinReleaseMatrix, discover_descriptors,
    publish_builtin_release_composition,
};
use lumvise_plugin_package::{
    HostCompatibility, PluginManifest, ReleaseComposition, install_verified_package,
    read_protected_signing_key, verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;

const TARGET_A: &str = "aarch64-apple-darwin";
const FUTURE_TARGET_B: &str = "x86_64-unknown-linux-gnu";

#[test]
fn repeated_matrix_publication_is_byte_identical() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let matrix = fixture_matrix(workspace.path());
    let key = SigningKey::from_bytes(&[97; 32]);
    let first = workspace.path().join("first");
    let second = workspace.path().join("second");

    let first_index =
        publish_builtin_release_composition(&matrix, &key, &first, ReleaseComposition::Full)
            .expect("first release");
    let second_index =
        publish_builtin_release_composition(&matrix, &key, &second, ReleaseComposition::Full)
            .expect("second release");
    assert_no_validation_directories(workspace.path());

    assert_eq!(first_index, second_index);
    for artifact in first_index.artifacts {
        assert_eq!(
            fs::read(first.join(&artifact.archive_path)).expect("first archive"),
            fs::read(second.join(&artifact.archive_path)).expect("second archive")
        );
    }
    assert_eq!(
        fs::read(first.join("builtins-release.json")).expect("first index"),
        fs::read(second.join("builtins-release.json")).expect("second index")
    );
}

#[test]
fn community_and_full_share_identical_signed_public_plugin_archives() {
    let workspace = tempfile::tempdir().unwrap();
    let matrix = fixture_matrix(workspace.path());
    let key = SigningKey::from_bytes(&[97; 32]);
    let full = workspace.path().join("full");
    let community = workspace.path().join("community");
    publish_builtin_release_composition(&matrix, &key, &full, ReleaseComposition::Full).unwrap();
    let index =
        publish_builtin_release_composition(&matrix, &key, &community, ReleaseComposition::Minimal)
            .unwrap();
    assert_eq!(index.artifacts.len(), 2);
    for artifact in index.artifacts {
        assert_eq!(
            fs::read(full.join(&artifact.archive_path)).unwrap(),
            fs::read(community.join(&artifact.archive_path)).unwrap()
        );
    }
}

#[cfg(unix)]
#[test]
fn prepared_release_cli_verifies_archives_with_the_supplied_key() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    let output = workspace.path().join("community");
    let key = SigningKey::from_bytes(&[97; 32]);
    let key_path = workspace.path().join("signing.key");
    fs::write(&key_path, key.to_bytes()).unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
    publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &key,
        &output,
        ReleaseComposition::Minimal,
    )
    .unwrap();
    let verify = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_lumvise-builtin-plugin-release"))
            .arg("--verify-release")
            .arg(&key_path)
            .arg(&output)
            .arg(TARGET_A)
            .output()
            .unwrap()
    };
    let accepted = verify();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&accepted.stdout).contains("verified 2 prepared plugin packages")
    );
    fs::write(&key_path, [98; 32]).unwrap();
    assert!(
        !verify().status.success(),
        "unrelated key must not authenticate a prepared release"
    );
}

fn assert_no_validation_directories(root: &Path) {
    let leaked = fs::read_dir(root)
        .expect("release workspace entries")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".builtin-install-"))
        .collect::<Vec<_>>();
    assert!(
        leaked.is_empty(),
        "immutable validation trees leaked: {leaked:?}"
    );
}

#[test]
fn complete_builtin_release_set_installs_for_current_host_protocol() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let key = SigningKey::from_bytes(&[101; 32]);
    let output = workspace.path().join("release");
    let index = publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &key,
        &output,
        ReleaseComposition::Full,
    )
    .expect("complete built-in release");
    let host = HostCompatibility::new(
        u32::from(lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION.major),
        TARGET_A,
    );

    for artifact in index.artifacts {
        let archive = output.join(artifact.archive_path);
        let verified =
            verify_package(&archive, &key.verifying_key(), &host).expect("verify built-in");
        install_verified_package(&verified, &workspace.path().join("installed"))
            .expect("install built-in");
    }
}
#[test]
fn explicit_compositions_publish_only_selected_packages_and_grants() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let key = SigningKey::from_bytes(&[102; 32]);
    let minimal_output = workspace.path().join("minimal");
    let minimal = publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &key,
        &minimal_output,
        ReleaseComposition::Minimal,
    )
    .expect("minimal release");

    assert_eq!(
        minimal
            .artifacts
            .iter()
            .map(|artifact| artifact.plugin_id.as_str())
            .collect::<Vec<_>>(),
        ["builtin.knowledge", "builtin.semantic"]
    );
    assert!(!minimal_output.join("builtin.assistant-0.1.6.lvp").exists());
    assert!(!minimal_output.join("builtin.nucleus-0.1.1.lvp").exists());
    let grants: serde_json::Value = serde_json::from_slice(
        &fs::read(minimal_output.join("host-capability-grants.json")).expect("minimal grants"),
    )
    .expect("parse minimal grants");
    assert!(
        grants["grants"]
            .as_array()
            .expect("grant list")
            .iter()
            .all(|grant| matches!(
                grant["plugin_id"].as_str(),
                Some("builtin.knowledge" | "builtin.semantic")
            ))
    );

    let full_output = workspace.path().join("full");
    let full = publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &key,
        &full_output,
        ReleaseComposition::Full,
    )
    .expect("full release");
    assert_eq!(
        full.artifacts.len(),
        fixture_matrix(workspace.path()).descriptors.len()
    );
    assert!(full_output.join("publisher-trust.json").is_file());
    for minimal_artifact in &minimal.artifacts {
        let full_artifact = full
            .artifacts
            .iter()
            .find(|artifact| artifact.plugin_id == minimal_artifact.plugin_id)
            .expect("minimal artifact must retain full identity");
        assert_eq!(minimal_artifact, full_artifact);
        assert_eq!(
            fs::read(minimal_output.join(&minimal_artifact.archive_path)).expect("minimal archive"),
            fs::read(full_output.join(&full_artifact.archive_path)).expect("full archive")
        );
    }
}

#[test]
fn generic_packager_preserves_future_multi_target_manifest() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let key = SigningKey::from_bytes(&[98; 32]);
    let output = workspace.path().join("release");
    let index = publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &key,
        &output,
        ReleaseComposition::Full,
    )
    .expect("matrix release");
    let semantic = index
        .artifacts
        .iter()
        .find(|item| item.plugin_id == "builtin.semantic")
        .expect("Semantic artifact");

    let verified = verify_package(
        &output.join(&semantic.archive_path),
        &key.verifying_key(),
        &HostCompatibility::new(CURRENT_PROTOCOL_VERSION.major as u32, FUTURE_TARGET_B),
    )
    .expect("verify Linux target");

    assert_eq!(
        verified
            .manifest()
            .targets
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![TARGET_A.to_owned(), FUTURE_TARGET_B.to_owned()]
    );
}

#[test]
fn post_signing_payload_tamper_is_rejected() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let key = SigningKey::from_bytes(&[99; 32]);
    let output = workspace.path().join("release");
    let index = publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &key,
        &output,
        ReleaseComposition::Full,
    )
    .expect("matrix release");
    let semantic = index
        .artifacts
        .iter()
        .find(|artifact| artifact.plugin_id == "builtin.semantic")
        .expect("semantic artifact");
    let archive = output.join(&semantic.archive_path);
    let mut bytes = fs::read(&archive).expect("archive bytes");
    let marker = b"builtin.semantic-aarch64-apple-darwin";
    let offsets = bytes
        .windows(marker.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == marker).then_some(offset))
        .collect::<Vec<_>>();
    let offset = *offsets.first().expect("semantic payload marker");
    bytes[offset] ^= 0x01;
    fs::write(&archive, bytes).expect("tampered archive");

    assert!(
        verify_package(
            &archive,
            &key.verifying_key(),
            &HostCompatibility::new(CURRENT_PROTOCOL_VERSION.major as u32, TARGET_A)
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn group_readable_release_key_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().expect("key workspace");
    let path = workspace.path().join("release.key");
    fs::write(&path, [1_u8; 32]).expect("key file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("key permissions");

    let error = read_protected_signing_key(&path).expect_err("insecure key rejected");

    assert!(error.to_string().contains("mode 640"));
}

#[test]
fn nonempty_release_destination_is_rejected_without_stale_leakage() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let output = workspace.path().join("release");
    fs::create_dir(&output).expect("release directory");
    fs::write(output.join("stale.lvp"), b"stale").expect("stale artifact");

    let error = publish_builtin_release_composition(
        &fixture_matrix(workspace.path()),
        &SigningKey::from_bytes(&[100; 32]),
        &output,
        ReleaseComposition::Full,
    )
    .expect_err("nonempty destination rejected");

    assert!(error.to_string().contains("missing or empty"));
    assert_eq!(fs::read(output.join("stale.lvp")).unwrap(), b"stale");
}

fn fixture_matrix(root: &Path) -> BuiltinReleaseMatrix {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("workspace root");
    let descriptors = discover_descriptors(workspace_root)
        .expect("discover descriptors")
        .into_iter()
        .map(|descriptor| (descriptor.plugin_id.clone(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let manifests = manifest_templates(workspace_root, &descriptors);
    BuiltinReleaseMatrix {
        targets: BTreeMap::from([
            (
                TARGET_A.into(),
                target_binaries(root, TARGET_A, &descriptors),
            ),
            (
                FUTURE_TARGET_B.into(),
                target_binaries(root, FUTURE_TARGET_B, &descriptors),
            ),
        ]),
        descriptors,
        manifests,
    }
}

fn target_binaries(
    root: &Path,
    target: &str,
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> BuiltinBinaryPaths {
    descriptors
        .keys()
        .map(|plugin_id| (plugin_id.clone(), binary(root, target, plugin_id)))
        .collect()
}

fn manifest_templates(
    workspace_root: &Path,
    descriptors: &BTreeMap<String, BuiltinReleaseDescriptor>,
) -> BTreeMap<String, PluginManifest> {
    descriptors
        .iter()
        .map(|(plugin_id, descriptor)| {
            let path = workspace_root
                .join(&descriptor.crate_path)
                .join(&descriptor.manifest_template);
            let bytes = fs::read(path).expect("manifest template");
            let manifest = serde_json::from_slice(&bytes).expect("parse manifest template");
            (plugin_id.clone(), manifest)
        })
        .collect()
}

fn binary(root: &Path, target: &str, plugin: &str) -> PathBuf {
    let path = root.join(target).join(plugin);
    fs::create_dir_all(path.parent().expect("binary parent")).expect("target directory");
    fs::write(&path, format!("{plugin}-{target}")).expect("fixture executable");
    path
}
