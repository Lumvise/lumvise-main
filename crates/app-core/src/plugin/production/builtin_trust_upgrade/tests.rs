use super::super::{PluginProductionConfig, tests::test_manifest};
use super::*;
use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{
    PluginReleaseArtifactV2, ReleaseComposition, build_package_from_directory,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

struct TrustUpgradeFixture {
    _workspace: tempfile::TempDir,
    state: PathBuf,
    old_release: PathBuf,
    new_release: PathBuf,
    development: SigningKey,
    production: SigningKey,
    config: PluginProductionConfig,
}

impl TrustUpgradeFixture {
    fn new() -> Self {
        let fixture = Self::fresh();
        fixture
            .config
            .install_bundled_release(&fixture.old_release)
            .unwrap();
        fixture
    }

    fn fresh() -> Self {
        let mut fixture = Self::empty();
        fixture.configure_test_pins();
        fixture.prepare_releases();
        fixture
    }

    fn empty() -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let state = workspace.path().join("state");
        Self {
            old_release: workspace.path().join("old"),
            new_release: workspace.path().join("new"),
            development: SigningKey::from_bytes(&[67; 32]),
            production: SigningKey::from_bytes(&[68; 32]),
            config: super::super::tests::test_production_config(state.clone()),
            state,
            _workspace: workspace,
        }
    }

    fn prepare_releases(&self) {
        self.write_release(
            &self.old_release,
            &self.development,
            DEVELOPMENT_KEY_ID,
            "0.1.3",
        );
        self.write_release(
            &self.new_release,
            &self.production,
            PRODUCTION_KEY_ID,
            "0.1.4",
        );
    }

    fn configure_test_pins(&mut self) {
        self.config.builtin_trust_upgrade = BuiltinTrustUpgrade::with_test_pins(
            hex::encode(self.development.verifying_key().to_bytes()),
            hex::encode(self.production.verifying_key().to_bytes()),
        );
    }

    fn write_release(&self, release: &Path, key: &SigningKey, key_id: &str, version: &str) {
        fs::create_dir_all(release).unwrap();
        let artifacts = ["builtin.knowledge", "builtin.semantic"]
            .map(|plugin_id| self.write_archive(release, key, key_id, version, plugin_id));
        self.write_index(release, artifacts.into());
        self.write_bundle_policies(release, key, key_id);
    }

    fn write_index(&self, release: &Path, artifacts: Vec<PluginReleaseArtifactV2>) {
        let index = PluginReleaseIndexV2 {
            schema_version: 2,
            product: "lumvise".into(),
            composition: ReleaseComposition::Minimal,
            artifacts,
        };
        fs::write(
            release.join("builtins-release.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
    }

    fn write_bundle_policies(&self, release: &Path, key: &SigningKey, key_id: &str) {
        let policy = json!({"schema_version":1,"keys":[key_record(key_id, key, false)]});
        self.write_release_trust(release, &policy);
        fs::write(
            release.join("host-capability-grants.json"),
            b"{\"schema_version\":1,\"grants\":[]}\n",
        )
        .unwrap();
    }

    fn write_release_trust(&self, release: &Path, policy: &Value) {
        fs::write(
            release.join("publisher-trust.json"),
            serde_json::to_vec(policy).unwrap(),
        )
        .unwrap();
    }

    fn change_bundled_grants(&self) {
        let grants = json!({"schema_version":1,"grants":[{"plugin_id":"builtin.semantic","capability_id":"storage.semantic"}]});
        fs::write(
            self.new_release.join("host-capability-grants.json"),
            serde_json::to_vec(&grants).unwrap(),
        )
        .unwrap();
    }

    fn write_archive(
        &self,
        release: &Path,
        key: &SigningKey,
        key_id: &str,
        version: &str,
        plugin_id: &str,
    ) -> PluginReleaseArtifactV2 {
        let payload = self.write_payload(release, plugin_id);
        let mut manifest = test_manifest(plugin_id, version);
        manifest.publisher.publisher_id = BUILTIN_PUBLISHER.into();
        manifest.publisher.key_id = key_id.into();
        let archive_path = format!("{plugin_id}-{version}.lvp");
        let archive = release.join(&archive_path);
        build_package_from_directory(&archive, manifest, &payload, key).unwrap();
        self.archive_record(plugin_id, version, key_id, archive_path, &archive)
    }

    fn write_payload(&self, release: &Path, plugin_id: &str) -> PathBuf {
        let payload = release.join(format!("{plugin_id}-payload"));
        fs::create_dir_all(payload.join("bin")).unwrap();
        fs::write(
            payload.join("bin/plugin"),
            b"compiled fixture, never executed",
        )
        .unwrap();
        payload
    }

    fn archive_record(
        &self,
        plugin_id: &str,
        version: &str,
        key_id: &str,
        archive_path: String,
        archive: &Path,
    ) -> PluginReleaseArtifactV2 {
        PluginReleaseArtifactV2 {
            plugin_id: plugin_id.into(),
            plugin_version: version.into(),
            publisher_id: BUILTIN_PUBLISHER.into(),
            key_id: key_id.into(),
            archive_path,
            archive_sha256: format!("{:x}", Sha256::digest(fs::read(archive).unwrap())),
            targets: vec!["test-target".into()],
        }
    }

    fn trust_path(&self) -> PathBuf {
        self.state.join("publisher-trust.json")
    }

    fn replace_semantic_archive(&self, signer: &SigningKey, key_id: &str) {
        let path = self.new_release.join("builtins-release.json");
        let mut index = PluginReleaseIndexV2::read(&path).unwrap();
        fs::remove_file(self.new_release.join(&index.artifacts[1].archive_path)).unwrap();
        index.artifacts[1] = self.write_archive(
            &self.new_release,
            signer,
            key_id,
            "0.1.4",
            "builtin.semantic",
        );
        fs::write(path, serde_json::to_vec(&index).unwrap()).unwrap();
    }

    fn policy(&self) -> Value {
        serde_json::from_slice(&fs::read(self.trust_path()).unwrap()).unwrap()
    }

    fn replace_policy(&self, policy: &Value) {
        fs::write(self.trust_path(), serde_json::to_vec(policy).unwrap()).unwrap();
    }

    fn install_new(&self) -> crate::Result<()> {
        self.config.install_bundled_release(&self.new_release)
    }

    fn assert_rejected_without_policy_change(&self) {
        let before = fs::read(self.trust_path()).unwrap();
        assert!(self.install_new().is_err());
        assert_eq!(fs::read(self.trust_path()).unwrap(), before);
        self.assert_no_policy_snapshots();
    }

    fn assert_no_policy_snapshots(&self) {
        assert!(fs::read_dir(&self.state).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".builtin-trust-upgrade-")
        }));
    }

    fn verifies_archive(&self, release: &Path, version: &str) -> bool {
        let trust = PublisherTrustStore::load_json_file(&self.trust_path()).unwrap();
        let host = HostCompatibility::new(1, "test-target");
        trust
            .verify_package(
                &release.join(format!("builtin.semantic-{version}.lvp")),
                &host,
            )
            .is_ok()
    }

    fn assert_selected_production_versions(&self) {
        let repository = self.config.repository.open().unwrap();
        let enabled: Vec<_> = repository
            .registry()
            .records
            .iter()
            .filter(|record| record.enabled)
            .collect();
        assert_eq!(enabled.len(), 2);
        assert!(enabled.iter().all(|record| record.version == "0.1.4"));
    }
}

fn key_record(key_id: &str, key: &SigningKey, revoked: bool) -> Value {
    json!({"publisher_id":BUILTIN_PUBLISHER,"key_id":key_id,
        "public_key_hex":hex::encode(key.verifying_key().to_bytes()),"revoked":revoked})
}

#[test]
fn old_install_accepts_pinned_bundle_and_rejects_development_authority() {
    let fixture = TrustUpgradeFixture::new();
    fixture.install_new().unwrap();
    assert!(fixture.verifies_archive(&fixture.new_release, "0.1.4"));
    assert!(!fixture.verifies_archive(&fixture.old_release, "0.1.3"));
    fixture.assert_selected_production_versions();
}

#[test]
fn administrator_keys_revocations_and_grant_bytes_are_preserved() {
    let fixture = TrustUpgradeFixture::new();
    let mut policy = fixture.policy();
    let custom = key_record("admin.custom", &SigningKey::from_bytes(&[69; 32]), false);
    let revoked = key_record("admin.revoked", &SigningKey::from_bytes(&[70; 32]), true);
    policy["keys"]
        .as_array_mut()
        .unwrap()
        .extend([custom.clone(), revoked.clone()]);
    fixture.replace_policy(&policy);
    let grants = fs::read(fixture.state.join("host-capability-grants.json")).unwrap();
    fixture.change_bundled_grants();
    fixture.install_new().unwrap();
    assert_eq!(&fixture.policy()["keys"][1], &custom);
    assert_eq!(&fixture.policy()["keys"][2], &revoked);
    assert_eq!(
        fs::read(fixture.state.join("host-capability-grants.json")).unwrap(),
        grants
    );
}

#[test]
fn intentional_denial_and_replaced_legacy_authority_are_unchanged() {
    for policy_kind in ["empty", "revoked", "replacement", "other-publisher"] {
        let fixture = TrustUpgradeFixture::new();
        let mut policy = fixture.policy();
        match policy_kind {
            "empty" => policy["keys"] = json!([]),
            "revoked" => policy["keys"][0]["revoked"] = json!(true),
            "replacement" => {
                policy["keys"][0] = key_record(
                    DEVELOPMENT_KEY_ID,
                    &SigningKey::from_bytes(&[71; 32]),
                    false,
                )
            }
            _ => policy["keys"][0]["publisher_id"] = json!("admin.publisher"),
        }
        fixture.replace_policy(&policy);
        fixture.assert_rejected_without_policy_change();
    }
}

#[test]
fn conflicting_or_revoked_production_authority_is_never_overridden() {
    for conflict in ["public-key", "revoked", "publisher"] {
        let fixture = TrustUpgradeFixture::new();
        let mut record = key_record(PRODUCTION_KEY_ID, &fixture.production, false);
        match conflict {
            "public-key" => {
                record = key_record(PRODUCTION_KEY_ID, &SigningKey::from_bytes(&[71; 32]), false)
            }
            "revoked" => record["revoked"] = json!(true),
            _ => record["publisher_id"] = json!("admin.publisher"),
        }
        let mut policy = fixture.policy();
        policy["keys"].as_array_mut().unwrap().push(record);
        fixture.replace_policy(&policy);
        fixture.assert_rejected_without_policy_change();
    }
}

#[test]
fn identical_existing_production_pin_is_reused() {
    let fixture = TrustUpgradeFixture::new();
    let mut policy = fixture.policy();
    policy["keys"].as_array_mut().unwrap().push(key_record(
        PRODUCTION_KEY_ID,
        &fixture.production,
        false,
    ));
    fixture.replace_policy(&policy);
    fixture.install_new().unwrap();
    assert_eq!(fixture.policy()["keys"].as_array().unwrap().len(), 2);
    assert_eq!(fixture.policy()["keys"][0]["revoked"], true);
}

#[test]
fn ordinary_development_bundle_does_not_migrate_trust() {
    let fixture = TrustUpgradeFixture::new();
    let before = fs::read(fixture.trust_path()).unwrap();
    fixture
        .config
        .install_bundled_release(&fixture.old_release)
        .unwrap();
    assert_eq!(fs::read(fixture.trust_path()).unwrap(), before);
}

#[test]
fn unpinned_or_revoked_bundle_leaves_installed_policy_unchanged() {
    for bundle_kind in ["absent", "wrong", "revoked"] {
        let fixture = TrustUpgradeFixture::new();
        let mut policy = json!({"schema_version":1,"keys":[key_record(PRODUCTION_KEY_ID, &fixture.production, false)]});
        match bundle_kind {
            "absent" => policy["keys"] = json!([]),
            "wrong" => {
                policy["keys"][0] =
                    key_record(PRODUCTION_KEY_ID, &SigningKey::from_bytes(&[71; 32]), false)
            }
            _ => policy["keys"][0]["revoked"] = json!(true),
        }
        fs::write(
            fixture.new_release.join("publisher-trust.json"),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        fixture.assert_rejected_without_policy_change();
    }
}

#[test]
fn tampered_archive_hash_leaves_installed_policy_unchanged() {
    let fixture = TrustUpgradeFixture::new();
    fs::write(
        fixture.new_release.join("builtin.semantic-0.1.4.lvp"),
        b"corrupt archive",
    )
    .unwrap();
    fixture.assert_rejected_without_policy_change();
}

#[test]
fn invalid_signature_with_matching_index_hash_does_not_revoke_legacy_key() {
    let fixture = TrustUpgradeFixture::new();
    let signer = SigningKey::from_bytes(&[71; 32]);
    fixture.replace_semantic_archive(&signer, PRODUCTION_KEY_ID);
    fixture.assert_rejected_without_policy_change();
}

#[test]
fn release_identity_mismatch_leaves_installed_policy_unchanged() {
    let fixture = TrustUpgradeFixture::new();
    let path = fixture.new_release.join("builtins-release.json");
    let mut index = PluginReleaseIndexV2::read(&path).unwrap();
    index.artifacts[1].key_id = DEVELOPMENT_KEY_ID.into();
    fs::write(path, serde_json::to_vec(&index).unwrap()).unwrap();
    fixture.assert_rejected_without_policy_change();
}

#[test]
fn strict_invalid_installed_policy_is_not_rewritten() {
    let fixture = TrustUpgradeFixture::new();
    let mut policy = fixture.policy();
    policy["unexpected"] = json!(true);
    fixture.replace_policy(&policy);
    fixture.assert_rejected_without_policy_change();
}

#[test]
fn migrated_restart_is_byte_identical() {
    let fixture = TrustUpgradeFixture::new();
    fixture.install_new().unwrap();
    let before = fs::read(fixture.trust_path()).unwrap();
    fixture.install_new().unwrap();
    assert_eq!(fs::read(fixture.trust_path()).unwrap(), before);
    fixture.assert_no_policy_snapshots();
}

#[test]
fn mixed_development_and_production_bundle_cannot_publish_revocation() {
    let fixture = TrustUpgradeFixture::new();
    fixture.replace_semantic_archive(&fixture.development, DEVELOPMENT_KEY_ID);
    let policy = json!({"schema_version":1,"keys":[
        key_record(PRODUCTION_KEY_ID, &fixture.production, false),
        key_record(DEVELOPMENT_KEY_ID, &fixture.development, false)]});
    fixture.write_release_trust(&fixture.new_release, &policy);
    fixture.assert_rejected_without_policy_change();
}

#[test]
fn changed_administrator_policy_is_not_replaced_by_prepared_snapshot() {
    let fixture = TrustUpgradeFixture::new();
    let original = fs::read(fixture.trust_path()).unwrap();
    let administrator = b"{\"schema_version\":1,\"keys\":[]}\n";
    fs::write(fixture.trust_path(), administrator).unwrap();
    let document = TrustUpgradeDocument::parse(&fixture.trust_path(), &original).unwrap();
    assert!(publish_policy(&fixture.trust_path(), &original, &document).is_err());
    assert_eq!(fs::read(fixture.trust_path()).unwrap(), administrator);
    fixture.assert_no_policy_snapshots();
}

#[test]
fn fresh_install_keeps_existing_initial_copy_path() {
    let fixture = TrustUpgradeFixture::fresh();
    fixture.install_new().unwrap();
    assert_eq!(
        fs::read(fixture.trust_path()).unwrap(),
        fs::read(fixture.new_release.join("publisher-trust.json")).unwrap()
    );
}

#[test]
fn install_io_failure_keeps_revocation_and_allows_retry() {
    let fixture = TrustUpgradeFixture::new();
    let repository = fixture.state.join("repository");
    let retained = fixture.state.join("retained-repository");
    fs::rename(&repository, &retained).unwrap();
    fs::write(&repository, b"blocked repository directory").unwrap();
    assert!(fixture.install_new().is_err());
    assert_eq!(fixture.policy()["keys"][0]["revoked"], true);
    let revoked = fs::read(fixture.trust_path()).unwrap();
    fs::remove_file(&repository).unwrap();
    fs::rename(retained, repository).unwrap();
    fixture.install_new().unwrap();
    assert_eq!(fs::read(fixture.trust_path()).unwrap(), revoked);
}

#[cfg(unix)]
#[test]
fn migrated_policy_has_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = TrustUpgradeFixture::new();
    fixture.install_new().unwrap();
    assert_eq!(
        fs::metadata(fixture.trust_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
