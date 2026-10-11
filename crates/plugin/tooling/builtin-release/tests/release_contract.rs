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
    HostCompatibility, InstalledPlugin, PluginManifest, ReleaseComposition,
    install_verified_package, read_protected_signing_key, remove_installed_package, verify_package,
};
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use sha2::{Digest, Sha256};

const TARGET_A: &str = "aarch64-apple-darwin";
const FUTURE_TARGET_B: &str = "x86_64-unknown-linux-gnu";
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
const RUNTIME_BYTES: &[u8] = b"reviewed-windows-runtime-unique-payload";
const RENDERER_PATH: &str = "views/runtime-fixture/index.html";

#[test]
fn repeated_matrix_publication_is_byte_identical() {
    let workspace = tempfile::tempdir().expect("release workspace");
    let matrix = fixture_matrix(workspace.path());
    let key = SigningKey::from_bytes(&[97; 32]);
    let first = workspace.path().join("first");
    let second = workspace.path().join("second");

    let first_index =
        publish_builtin_release_composition(&matrix, &key, &first, ReleaseComposition::Full, None)
            .expect("first release");
    let second_index =
        publish_builtin_release_composition(&matrix, &key, &second, ReleaseComposition::Full, None)
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
    publish_builtin_release_composition(&matrix, &key, &full, ReleaseComposition::Full, None)
        .unwrap();
    let index = publish_builtin_release_composition(
        &matrix,
        &key,
        &community,
        ReleaseComposition::Minimal,
        None,
    )
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
        None,
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
        None,
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
        None,
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
        None,
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
        None,
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
        None,
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
        None,
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

#[test]
fn windows_companions_are_signed_installable_and_adjacent_only_to_windows_executables() {
    let fixture = WindowsReleaseFixture::new();
    let index = fixture.publish("full", ReleaseComposition::Full);
    for artifact in index.artifacts {
        let archive = fixture.root.path().join("full").join(artifact.archive_path);
        assert_windows_install(&fixture, &archive);
        assert_non_windows_install(&fixture, &archive, TARGET_A);
        assert_non_windows_install(&fixture, &archive, FUTURE_TARGET_B);
    }
    assert_eq!(
        fs::read(fixture.runtime.join("VCRUNTIME140.dll")).unwrap(),
        RUNTIME_BYTES
    );
}

#[test]
fn windows_without_companions_preserves_existing_file_tree() {
    let fixture = WindowsReleaseFixture::new();
    let output = fixture.root.path().join("without");
    let index = publish_builtin_release_composition(
        &fixture.matrix,
        &fixture.key,
        &output,
        ReleaseComposition::Full,
        None,
    )
    .unwrap();
    for artifact in index.artifacts {
        assert_archive_without_dlls(&fixture, &output.join(artifact.archive_path));
    }
}

#[test]
fn full_minimal_and_repeated_publication_share_identical_companion_archives() {
    let fixture = WindowsReleaseFixture::new();
    fixture.publish("full", ReleaseComposition::Full);
    fixture.publish("repeat", ReleaseComposition::Full);
    let minimal = fixture.publish("minimal", ReleaseComposition::Minimal);
    for artifact in minimal.artifacts {
        assert_matching_companion_archives(fixture.root.path(), &artifact.archive_path);
    }
}

#[test]
fn signed_companion_tampering_is_rejected_for_every_host() {
    let fixture = WindowsReleaseFixture::new();
    let index = fixture.publish("full", ReleaseComposition::Full);
    let archive = fixture
        .root
        .path()
        .join("full")
        .join(&index.artifacts[0].archive_path);
    tamper_runtime_bytes(&archive);
    for target in [WINDOWS_TARGET, TARGET_A, FUTURE_TARGET_B] {
        assert!(
            verify_package(
                &archive,
                &fixture.key.verifying_key(),
                &release_host(target)
            )
            .is_err()
        );
    }
}

#[test]
fn companion_input_requires_a_windows_matrix_target() {
    let mut fixture = WindowsReleaseFixture::new();
    fixture.matrix.targets.remove(WINDOWS_TARGET);
    fixture.assert_rejected(WINDOWS_TARGET);
}

#[test]
fn absent_companion_directory_is_rejected() {
    let fixture = WindowsReleaseFixture::new();
    fs::remove_dir_all(&fixture.runtime).unwrap();
    fixture.assert_rejected("runtime");
}

#[test]
fn regular_file_instead_of_companion_directory_is_rejected() {
    let fixture = WindowsReleaseFixture::new();
    fs::remove_dir_all(&fixture.runtime).unwrap();
    fs::write(&fixture.runtime, b"not a directory").unwrap();
    fixture.assert_rejected("non-symlink DLL directory");
}

#[test]
fn empty_companion_directory_is_rejected() {
    let fixture = WindowsReleaseFixture::new();
    empty_runtime(&fixture.runtime);
    fixture.assert_rejected("at least one nonempty regular *.dll");
}

#[test]
fn nested_dlls_are_not_collected_from_companion_directory() {
    let fixture = WindowsReleaseFixture::new();
    empty_runtime(&fixture.runtime);
    fs::create_dir(fixture.runtime.join("nested")).unwrap();
    fs::write(fixture.runtime.join("nested/runtime.dll"), RUNTIME_BYTES).unwrap();
    fs::write(fixture.runtime.join("README.txt"), b"reviewed directory").unwrap();
    fixture.assert_rejected("at least one nonempty regular *.dll");
}

#[test]
fn valid_runtime_directory_adds_only_immediate_dll_files() {
    let fixture = WindowsReleaseFixture::new();
    fs::create_dir(fixture.runtime.join("nested")).unwrap();
    fs::write(fixture.runtime.join("nested/ignored.dll"), b"nested").unwrap();
    fs::write(fixture.runtime.join("README.txt"), b"reviewed directory").unwrap();
    let index = fixture.publish("full", ReleaseComposition::Full);
    for artifact in index.artifacts {
        let archive = fixture.root.path().join("full").join(artifact.archive_path);
        assert_only_immediate_runtime_dlls(&fixture, &archive);
    }
}

#[test]
fn empty_companion_file_is_rejected() {
    let fixture = WindowsReleaseFixture::new();
    fs::write(fixture.runtime.join("VCRUNTIME140.dll"), b"").unwrap();
    fixture.assert_rejected("nonempty DLL file");
}

#[test]
fn directory_with_dll_basename_is_rejected() {
    let fixture = WindowsReleaseFixture::new();
    fs::create_dir(fixture.runtime.join("directory.dll")).unwrap();
    fixture.assert_rejected("regular non-symlink DLL file");
}

#[cfg(unix)]
const INVALID_DLL_BASENAMES: &[&str] = &[
    ".dll",
    "CON.dll",
    "NUL.extra.dll",
    "COM1.dll",
    "LPT².dll",
    "bad:name.dll",
    "bad<name.dll",
    "bad>name.dll",
    "bad\\name.dll",
    "bad|name.dll",
    "bad?name.dll",
    "bad*name.dll",
    "bad\"name.dll",
    "bad\u{1f}name.dll",
    "trailing.dll.",
    "trailing.dll ",
];

#[cfg(unix)]
#[test]
fn malformed_windows_companion_basenames_are_rejected() {
    for name in INVALID_DLL_BASENAMES {
        let fixture = WindowsReleaseFixture::new();
        fs::write(fixture.runtime.join(name), RUNTIME_BYTES).unwrap();
        fixture.assert_rejected("Windows DLL basename");
    }
}

#[cfg(unix)]
#[test]
fn symlinked_companion_is_rejected() {
    let fixture = WindowsReleaseFixture::new();
    std::os::unix::fs::symlink(
        fixture.runtime.join("VCRUNTIME140.dll"),
        fixture.runtime.join("linked.dll"),
    )
    .unwrap();
    fixture.assert_rejected("regular non-symlink DLL file");
}

#[cfg(unix)]
#[test]
fn symlinked_companion_directory_is_rejected() {
    let mut fixture = WindowsReleaseFixture::new();
    let link = fixture.root.path().join("runtime-link");
    std::os::unix::fs::symlink(&fixture.runtime, &link).unwrap();
    fixture.runtime = link;
    fixture.assert_rejected("non-symlink DLL directory");
}

#[test]
fn case_colliding_companion_files_are_rejected_when_filesystem_supports_them() {
    let fixture = WindowsReleaseFixture::new();
    fs::write(fixture.runtime.join("vcruntime140.DLL"), RUNTIME_BYTES).unwrap();
    if fs::read_dir(&fixture.runtime).unwrap().count() == 2 {
        // The pure basename rule is also tested on case-insensitive hosts.
        eprintln!("case-insensitive filesystem cannot represent duplicate DLL entries");
        return;
    }
    fixture.assert_rejected("case-insensitively unique DLL basenames");
}

#[test]
fn companion_cannot_overwrite_case_colliding_executable() {
    let mut fixture = WindowsReleaseFixture::new();
    fixture
        .matrix
        .descriptors
        .get_mut("builtin.semantic")
        .unwrap()
        .payload_pattern = "plugins/builtin.semantic/{target}/vcruntime140.DLL".into();
    fixture.assert_rejected("conflicts with");
}

#[test]
fn companion_cannot_overwrite_renderer_asset() {
    let mut fixture = WindowsReleaseFixture::new();
    fixture.colliding_asset("vcruntime140.DLL");
    fixture.assert_rejected("conflicts with");
}

#[test]
fn companion_cannot_replace_asset_directory_or_descend_through_asset_file() {
    let mut fixture = WindowsReleaseFixture::new();
    fixture.colliding_asset("vcruntime140.DLL/index.html");
    fixture.assert_rejected("conflicts with");
    let mut fixture = WindowsReleaseFixture::new();
    add_ancestor_asset_collision(&mut fixture);
    fixture.assert_rejected("conflicts with");
}

struct WindowsReleaseFixture {
    root: tempfile::TempDir,
    matrix: BuiltinReleaseMatrix,
    runtime: PathBuf,
    key: SigningKey,
}

impl WindowsReleaseFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut matrix = fixture_matrix(root.path());
        matrix.targets.insert(
            WINDOWS_TARGET.into(),
            target_binaries(root.path(), WINDOWS_TARGET, &matrix.descriptors),
        );
        add_renderer_fixture(&mut matrix, root.path());
        let runtime = root.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::write(runtime.join("VCRUNTIME140.dll"), RUNTIME_BYTES).unwrap();
        fs::write(runtime.join("DirectML.DLL"), b"reviewed-directml").unwrap();
        Self {
            root,
            matrix,
            runtime,
            key: SigningKey::from_bytes(&[103; 32]),
        }
    }

    fn publish(
        &self,
        directory: &str,
        composition: ReleaseComposition,
    ) -> lumvise_plugin_package::PluginReleaseIndexV2 {
        publish_builtin_release_composition(
            &self.matrix,
            &self.key,
            &self.root.path().join(directory),
            composition,
            Some(&self.runtime),
        )
        .unwrap()
    }

    fn assert_rejected(&self, expected: &str) {
        let output = self.root.path().join("rejected");
        let error = publish_builtin_release_composition(
            &self.matrix,
            &self.key,
            &output,
            ReleaseComposition::Full,
            Some(&self.runtime),
        )
        .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!output.exists(), "failed release became visible");
        assert_no_validation_directories(self.root.path());
        assert_no_release_directories(self.root.path());
    }

    fn colliding_asset(&mut self, name: &str) {
        let descriptor = self.matrix.descriptors.get_mut("builtin.semantic").unwrap();
        let executable = descriptor
            .payload_pattern
            .replace("{target}", WINDOWS_TARGET);
        descriptor.payload_assets_dir = Some(executable.rsplit_once('/').unwrap().0.into());
        let path = descriptor.payload_assets_path.as_ref().unwrap().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"renderer asset").unwrap();
    }
}

fn add_renderer_fixture(matrix: &mut BuiltinReleaseMatrix, root: &Path) {
    let descriptor = matrix.descriptors.get_mut("builtin.semantic").unwrap();
    let assets = root.join("renderer");
    fs::create_dir(&assets).unwrap();
    fs::write(assets.join("index.html"), b"existing renderer asset").unwrap();
    descriptor.payload_assets_dir = Some("views/runtime-fixture".into());
    descriptor.payload_assets_path = Some(assets);
}

fn empty_runtime(runtime: &Path) {
    fs::remove_dir_all(runtime).unwrap();
    fs::create_dir(runtime).unwrap();
}

fn release_host(target: &str) -> HostCompatibility {
    HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), target)
}

fn assert_windows_install(fixture: &WindowsReleaseFixture, archive: &Path) {
    let installed = verified_fixture_install(fixture, archive, WINDOWS_TARGET);
    assert_installed_library(&installed, "VCRUNTIME140.dll", RUNTIME_BYTES);
    assert_installed_library(&installed, "DirectML.DLL", b"reviewed-directml");
    if installed.plugin_id() == "builtin.semantic" {
        assert_eq!(
            fs::read(installed.root().join(RENDERER_PATH)).unwrap(),
            b"existing renderer asset"
        );
        assert!(installed.file_sha256(RENDERER_PATH).is_some());
    }
    remove_installed_package(&installed).unwrap();
}

fn assert_installed_library(installed: &InstalledPlugin, name: &str, bytes: &[u8]) {
    let path = installed.executable().parent().unwrap().join(name);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let relative = path
        .strip_prefix(installed.root())
        .unwrap()
        .to_str()
        .unwrap()
        .replace('\\', "/");
    assert_eq!(
        installed.file_sha256(&relative),
        Some(hex::encode(Sha256::digest(bytes)).as_str())
    );
}

fn verified_fixture_install(
    fixture: &WindowsReleaseFixture,
    archive: &Path,
    target: &str,
) -> InstalledPlugin {
    let verified =
        verify_package(archive, &fixture.key.verifying_key(), &release_host(target)).unwrap();
    install_verified_package(
        &verified,
        &fixture.root.path().join(format!("installed-{target}")),
    )
    .unwrap()
}

fn assert_non_windows_install(fixture: &WindowsReleaseFixture, archive: &Path, target: &str) {
    let installed = verified_fixture_install(fixture, archive, target);
    for name in ["VCRUNTIME140.dll", "DirectML.DLL"] {
        assert!(!installed.executable().parent().unwrap().join(name).exists());
    }
    remove_installed_package(&installed).unwrap();
}

fn assert_archive_without_dlls(fixture: &WindowsReleaseFixture, archive: &Path) {
    let verified = verify_package(
        archive,
        &fixture.key.verifying_key(),
        &release_host(WINDOWS_TARGET),
    )
    .unwrap();
    assert!(
        !verified
            .manifest()
            .files
            .keys()
            .any(|path| path.ends_with(".dll"))
    );
}

fn assert_matching_companion_archives(root: &Path, archive: &str) {
    let bytes = fs::read(root.join("full").join(archive)).unwrap();
    for edition in ["repeat", "minimal"] {
        assert_eq!(bytes, fs::read(root.join(edition).join(archive)).unwrap());
    }
}

fn tamper_runtime_bytes(archive: &Path) {
    let mut bytes = fs::read(archive).unwrap();
    let offset = bytes
        .windows(RUNTIME_BYTES.len())
        .position(|window| window == RUNTIME_BYTES)
        .unwrap();
    bytes[offset] ^= 1;
    fs::write(archive, bytes).unwrap();
}

fn add_ancestor_asset_collision(fixture: &mut WindowsReleaseFixture) {
    let descriptor = fixture
        .matrix
        .descriptors
        .get_mut("builtin.semantic")
        .unwrap();
    descriptor.payload_pattern = format!("views/runtime-fixture/{WINDOWS_TARGET}/plugin");
    let path = descriptor
        .payload_assets_path
        .as_ref()
        .unwrap()
        .join(WINDOWS_TARGET);
    fs::write(path, b"asset file").unwrap();
}

fn assert_no_release_directories(root: &Path) {
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".builtin-release-")
    }));
}

fn assert_only_immediate_runtime_dlls(fixture: &WindowsReleaseFixture, archive: &Path) {
    let verified = verify_package(
        archive,
        &fixture.key.verifying_key(),
        &release_host(WINDOWS_TARGET),
    )
    .unwrap();
    let paths = verified.manifest().files.keys().collect::<Vec<_>>();
    let dll_count = paths
        .iter()
        .filter(|path| path.to_ascii_lowercase().ends_with(".dll"))
        .count();
    assert_eq!(dll_count, 2);
    assert!(
        !paths
            .iter()
            .any(|path| path.contains("README") || path.contains("ignored"))
    );
}
