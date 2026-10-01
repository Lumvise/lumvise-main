//! Public release API proof: independent source workspaces share one release matrix.

use lumvise_builtin_plugin_release::{
    BuiltinCompilationProfile, build_builtin_binaries_composition_with_profile,
    discover_release_descriptors,
};
use lumvise_plugin_package::ReleaseComposition;
use lumvise_plugin_protocol::CURRENT_PROTOCOL_VERSION;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct ReleaseSourceFixture {
    root: tempfile::TempDir,
}

impl ReleaseSourceFixture {
    fn new(identity: &str) -> Self {
        let root = tempfile::tempdir().expect("source workspace");
        let member = root.path().join("crates/plugin/builtins/fixture");
        fs::create_dir_all(member.join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"crates/plugin/builtins/fixture\"]\n",
        )
        .unwrap();
        fs::write(member.join("Cargo.toml"), format!("[package]\nname = \"fixture-{identity}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[[bin]]\nname = \"fixture-{identity}\"\npath = \"src/main.rs\"\n")).unwrap();
        fs::write(member.join("src/main.rs"), "fn main() {}\n").unwrap();
        Self::write_descriptor(&member, identity);
        Self::write_manifest(&member, identity);
        Self { root }
    }

    fn path(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    fn write_descriptor(member: &Path, identity: &str) {
        let major = CURRENT_PROTOCOL_VERSION.major;
        fs::write(member.join("lumvise-builtin-release.toml"), format!(
            "package = \"fixture-{identity}\"\nbinary = \"fixture-{identity}\"\nplugin_id = \"builtin.{identity}\"\nplugin_version = \"0.1.0\"\npublisher_id = \"fixture.publisher\"\nkey_id = \"fixture.key\"\nprotocol_min = {major}\nprotocol_max = {major}\npayload_pattern = \"bin/{{target}}/fixture-{identity}\"\nmanifest_template = \"manifest.json\"\n"
        )).unwrap();
    }

    fn write_manifest(member: &Path, identity: &str) {
        fs::write(member.join("manifest.json"), serde_json::to_vec(&json!({
            "schema_version":1, "publisher":{"publisher_id":"fixture.publisher","key_id":"fixture.key"},
            "plugin_id":format!("builtin.{identity}"), "plugin_version":"0.1.0",
            "protocol":{"min":CURRENT_PROTOCOL_VERSION.major,"max":CURRENT_PROTOCOL_VERSION.major},
            "targets":{}, "files":{}, "exports":[], "host_capabilities":[]
        })).unwrap()).unwrap();
    }

    fn lock(&self) {
        let status = Command::new(env!("CARGO"))
            .args(["generate-lockfile", "--offline"])
            .arg("--manifest-path")
            .arg(self.root.path().join("Cargo.toml"))
            .status()
            .unwrap();
        assert!(status.success(), "fixture lock generation: {status}");
    }
}

#[test]
fn descriptors_resolve_to_their_own_workspace_without_publishing_private_paths() {
    let public = ReleaseSourceFixture::new("knowledge");
    let private = ReleaseSourceFixture::new("assistant");
    let descriptors = discover_release_descriptors(&[public.path(), private.path()]).unwrap();
    assert_eq!(descriptors.len(), 2);
    assert_eq!(descriptors[0].plugin_id, "builtin.assistant");
    assert_eq!(
        descriptors[0].workspace_root,
        private.path().canonicalize().unwrap()
    );
    assert_eq!(
        descriptors[1].workspace_root,
        public.path().canonicalize().unwrap()
    );
    let serialized = serde_json::to_value(&descriptors).unwrap();
    assert!(serialized[0].get("workspace_root").is_none());
}

#[test]
fn combined_release_rejects_duplicate_identities_and_no_sources() {
    let first = ReleaseSourceFixture::new("knowledge");
    let duplicate = ReleaseSourceFixture::new("knowledge");
    let error = discover_release_descriptors(&[first.path(), duplicate.path()]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unique package names, binary names and plugin IDs")
    );
    assert!(
        discover_release_descriptors(&[])
            .unwrap_err()
            .to_string()
            .contains("source workspace")
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn independently_locked_workspaces_compile_into_one_community_matrix() {
    let knowledge = ReleaseSourceFixture::new("knowledge");
    let semantic = ReleaseSourceFixture::new("semantic");
    knowledge.lock();
    semantic.lock();
    let cache = tempfile::tempdir().unwrap();
    let matrix = build_builtin_binaries_composition_with_profile(
        &[knowledge.path(), semantic.path()],
        &["aarch64-apple-darwin".into()],
        cache.path(),
        BuiltinCompilationProfile::Debug,
        ReleaseComposition::Minimal,
    )
    .expect("compile independent workspaces");
    assert_eq!(matrix.descriptors.len(), 2);
    assert_eq!(matrix.targets["aarch64-apple-darwin"].len(), 2);
    for (id, binary) in &matrix.targets["aarch64-apple-darwin"] {
        assert!(binary.is_file(), "missing {id}: {}", binary.display());
        assert!(binary.starts_with(cache.path()));
        assert!(Command::new(binary).status().unwrap().success());
    }
}
