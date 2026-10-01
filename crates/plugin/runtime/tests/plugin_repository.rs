#![cfg(unix)]
#![expect(
    dead_code,
    unused_imports,
    reason = "shared integration support intentionally serves lifecycle and repository binaries"
)]

include!("support/mod.rs");
use lumvise_plugin_runtime::PluginRestoreFailureKind;

fn versioned_archive(fixture: &InstalledFixture, version: &str) -> PathBuf {
    let executable =
        std::fs::read(env!("CARGO_BIN_EXE_plugin-runtime-fixture")).expect("fixture executable");
    let mut manifest = fixture_manifest(fixture.package.plugin_id(), &executable);
    manifest.plugin_version = version.to_owned();
    let mut marker = manifest.exports[0].clone();
    marker.id = format!("fixture.version.{}", version.replace('+', "."));
    manifest.exports.push(marker);
    let archive = fixture.workspace.path().join(format!("{version}.lvp"));
    lumvise_plugin_package::build_package(
        &archive,
        lumvise_plugin_package::BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(EXECUTABLE_PATH.into(), executable)]),
            signing_key: &fixture.signing_key,
        },
    )
    .expect("signed versioned archive");
    archive
}

#[test]
fn bundled_archive_rejects_same_version_with_different_signed_content() {
    let fixture = InstalledFixture::new("repository-bundle-collision");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    let original = repository
        .install_bundled_archive(&fixture.archive)
        .expect("first bundled archive");
    let repeated = repository
        .install_bundled_archive(&fixture.archive)
        .expect("exact-content idempotency");
    assert_eq!(repeated, original);
    let collision = versioned_archive(&fixture, "1.0.0");
    let error = repository
        .install_bundled_archive(&collision)
        .expect_err("signed same-version collision");
    assert!(matches!(
        error,
        PluginRepositoryError::VersionCollision { .. }
    ));
    assert_eq!(repository.registry().records, [original]);
    make_writable(root.path());
}

#[test]
fn validation_failure_restores_prior_runtime_catalog_and_floor() {
    let fixture = InstalledFixture::new("repository-validation-rollback");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    repository
        .install_archive(&fixture.archive, true)
        .expect("select original");
    let newer = versioned_archive(&fixture, "2.0.0");
    repository
        .install_archive(&newer, false)
        .expect("stage update");
    let system = test_plugin_system();
    repository.restore_into(&system).expect("start original");
    let mut calls = 0;

    let error = repository
        .activate_with_validation(&system, "repository-validation-rollback", "2.0.0", || {
            calls += 1;
            if calls == 1 {
                Err("catalog rejected candidate".into())
            } else {
                Ok(())
            }
        })
        .expect_err("candidate validation fails");

    assert!(matches!(
        error,
        PluginRepositoryError::ActivationValidation { .. }
    ));
    assert_eq!(calls, 2, "rollback must resync the prior catalog");
    assert!(system.is_active("repository-validation-rollback").unwrap());
    assert!(
        !system.published_plugins().unwrap()[0]
            .exports
            .iter()
            .any(|export| export.id == "fixture.version.2.0.0")
    );
    assert_eq!(
        repository.registry().highest_enabled_versions["repository-validation-rollback"],
        "1.0.0"
    );
    make_writable(root.path());
}

#[test]
fn rollback_reports_original_validation_and_prior_catalog_failure() {
    let fixture = InstalledFixture::new("repository-validation-report");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    repository
        .install_archive(&fixture.archive, true)
        .expect("select original");
    let newer = versioned_archive(&fixture, "2.0.0");
    repository
        .install_archive(&newer, false)
        .expect("stage update");
    let system = test_plugin_system();
    repository.restore_into(&system).expect("start original");

    let error = repository
        .activate_with_validation(&system, "repository-validation-report", "2.0.0", || {
            Err("catalog unavailable".into())
        })
        .expect_err("both catalog attempts fail");

    let message = error.to_string();
    assert!(
        message.contains("activation validation failed"),
        "{message}"
    );
    assert!(
        message.contains("restoring prior catalog: catalog unavailable"),
        "{message}"
    );
    assert!(system.is_active("repository-validation-report").unwrap());
    assert_eq!(
        repository.registry().highest_enabled_versions["repository-validation-report"],
        "1.0.0"
    );
    make_writable(root.path());
}

#[test]
fn enabled_version_floor_survives_reopen_disable_and_uninstall() {
    let fixture = InstalledFixture::new("repository-floor");
    let root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    let newer = versioned_archive(&fixture, "1.10.0");
    let mut repository = open_fixture_repository(root.path(), Arc::clone(&trust));
    repository
        .install_archive(&fixture.archive, true)
        .expect("initial selection");
    repository
        .install_archive(&newer, false)
        .expect("download update");
    let system = test_plugin_system();
    repository
        .activate(&system, "repository-floor", "1.10.0")
        .expect("activate update");
    repository
        .disable("repository-floor")
        .expect("disable plugin");
    repository
        .uninstall("repository-floor", "1.10.0", UninstallPolicy::Purge)
        .expect("remove selected version");
    drop(repository);

    let mut reopened = open_fixture_repository(root.path(), trust);
    assert_eq!(
        reopened.registry().highest_enabled_versions["repository-floor"],
        "1.10.0"
    );
    assert!(reopened.enable("repository-floor", "1.0.0").is_err());
    assert!(reopened.install_bundled_archive(&fixture.archive).is_ok());
    assert!(reopened.install_bundled_archive(&fixture.archive).is_ok());
    assert!(!reopened.registry().records[0].enabled);
    make_writable(root.path());
}

#[test]
fn direct_lifecycle_rejects_downgrade_before_runtime_or_archive_mutation() {
    let fixture = InstalledFixture::new("repository-direct-floor");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    let newer = versioned_archive(&fixture, "1.10.0");
    repository
        .install_archive(&fixture.archive, false)
        .expect("older candidate");
    repository
        .install_archive(&newer, true)
        .expect("select newer");
    let system = test_plugin_system();
    assert!(
        repository
            .enable("repository-direct-floor", "1.0.0")
            .is_err()
    );
    assert!(
        repository
            .activate(&system, "repository-direct-floor", "1.0.0")
            .is_err()
    );
    assert!(!system.is_installed("repository-direct-floor").unwrap());
    repository
        .uninstall("repository-direct-floor", "1.0.0", UninstallPolicy::Purge)
        .expect("remove old candidate");
    assert!(repository.install_archive(&fixture.archive, true).is_err());
    assert_eq!(
        repository.registry().highest_enabled_versions["repository-direct-floor"],
        "1.10.0"
    );
    assert_eq!(repository.registry().records.len(), 1);
    make_writable(root.path());
}

#[test]
fn bundled_semver_precedence_preserves_selected_version_and_disabled_intent() {
    let fixture = InstalledFixture::new("repository-semver");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    let prerelease = versioned_archive(&fixture, "1.9.0-rc.1");
    let release = versioned_archive(&fixture, "1.9.0");
    let newer = versioned_archive(&fixture, "1.10.0+build.1");
    repository
        .install_bundled_archive(&prerelease)
        .expect("first bundled release");
    repository
        .install_bundled_archive(&release)
        .expect("release supersedes prerelease");
    repository
        .install_bundled_archive(&newer)
        .expect("numeric newer version");
    repository
        .install_bundled_archive(&fixture.archive)
        .expect("stale bundle");
    assert_eq!(
        repository.registry().highest_enabled_versions["repository-semver"],
        "1.10.0+build.1"
    );
    assert_eq!(
        repository
            .registry()
            .records
            .iter()
            .find(|record| record.enabled)
            .unwrap()
            .version,
        "1.10.0+build.1"
    );
    repository
        .disable("repository-semver")
        .expect("user disables plugin");
    let future = versioned_archive(&fixture, "1.11.0");
    repository
        .install_bundled_archive(&future)
        .expect("future bundle remains disabled");
    assert!(
        repository
            .registry()
            .records
            .iter()
            .all(|record| !record.enabled)
    );
    make_writable(root.path());
}

#[test]
fn legacy_registry_seeds_only_enabled_versions_and_rejects_below_floor_active_record() {
    let fixture = InstalledFixture::new("repository-legacy-floor");
    let root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    let newer = versioned_archive(&fixture, "2.0.0");
    let mut repository = open_fixture_repository(root.path(), Arc::clone(&trust));
    repository
        .install_archive(&fixture.archive, true)
        .expect("selected version");
    repository
        .install_archive(&newer, false)
        .expect("downloaded candidate");
    drop(repository);
    let registry_path = root.path().join("registry.json");
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    snapshot
        .as_object_mut()
        .unwrap()
        .remove("highest_enabled_versions");
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&snapshot).unwrap(),
    )
    .unwrap();

    let reopened = open_fixture_repository(root.path(), Arc::clone(&trust));
    assert_eq!(
        reopened.registry().highest_enabled_versions["repository-legacy-floor"],
        "1.0.0"
    );
    drop(reopened);
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    assert_eq!(
        persisted["highest_enabled_versions"]["repository-legacy-floor"],
        "1.0.0"
    );
    snapshot["highest_enabled_versions"] = json!({"repository-legacy-floor": "2.0.0"});
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&snapshot).unwrap(),
    )
    .unwrap();
    assert!(
        PluginRepository::open(
            root.path(),
            trust,
            HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
        )
        .is_err()
    );
    make_writable(root.path());
}

#[test]
fn failed_activation_commit_restores_previous_runtime_and_floor() {
    let fixture = InstalledFixture::new("repository-activation-rollback");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    repository
        .install_archive(&fixture.archive, true)
        .expect("select original");
    let newer = versioned_archive(&fixture, "2.0.0");
    repository
        .install_archive(&newer, false)
        .expect("stage update");
    let system = test_plugin_system();
    repository.restore_into(&system).expect("start original");
    let registry_path = root.path().join("registry.json");
    let snapshot = std::fs::read(&registry_path).expect("saved snapshot");
    std::fs::remove_file(&registry_path).expect("remove registry snapshot");
    std::fs::create_dir(&registry_path).expect("block atomic commit");

    let mut validation_calls = 0;
    assert!(
        repository
            .activate_with_validation(&system, "repository-activation-rollback", "2.0.0", || {
                validation_calls += 1;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(
        validation_calls, 2,
        "candidate and restored catalog validated"
    );
    assert!(system.is_active("repository-activation-rollback").unwrap());
    assert!(
        !system.published_plugins().unwrap()[0]
            .exports
            .iter()
            .any(|export| export.id == "fixture.version.2.0.0")
    );
    assert_eq!(
        repository.registry().highest_enabled_versions["repository-activation-rollback"],
        "1.0.0"
    );
    assert_eq!(
        repository
            .registry()
            .records
            .iter()
            .find(|record| record.enabled)
            .unwrap()
            .version,
        "1.0.0"
    );
    std::fs::remove_dir(&registry_path).expect("unblock registry");
    std::fs::write(&registry_path, snapshot).expect("restore saved snapshot");
    make_writable(root.path());
}

#[test]
fn failed_enabled_install_keeps_previous_snapshot_and_floor() {
    let fixture = InstalledFixture::new("repository-install-rollback");
    let root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(root.path(), fixture_trust(&fixture));
    repository
        .install_archive(&fixture.archive, true)
        .expect("select original");
    let newer = versioned_archive(&fixture, "2.0.0");
    let registry_path = root.path().join("registry.json");
    let snapshot = std::fs::read(&registry_path).expect("saved snapshot");
    std::fs::remove_file(&registry_path).expect("remove registry snapshot");
    std::fs::create_dir(&registry_path).expect("block atomic commit");

    assert!(repository.install_archive(&newer, true).is_err());
    assert_eq!(repository.registry().records.len(), 1);
    assert_eq!(
        repository.registry().highest_enabled_versions["repository-install-rollback"],
        "1.0.0"
    );
    std::fs::remove_dir(&registry_path).expect("unblock registry");
    std::fs::write(&registry_path, snapshot).expect("restore saved snapshot");
    make_writable(root.path());
}

#[test]
fn enabled_repository_record_reverifies_and_starts_after_restart() {
    let fixture = InstalledFixture::new("repository-enabled");
    let repository_root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    let mut repository = open_fixture_repository(repository_root.path(), Arc::clone(&trust));
    repository
        .install_archive(&fixture.archive, true)
        .expect("register enabled package");
    drop(repository);
    let restored = open_fixture_repository(repository_root.path(), trust);
    let system = test_plugin_system();

    restored
        .restore_into(&system)
        .expect("restore enabled package");

    assert!(
        system
            .is_active("repository-enabled")
            .expect("active catalog")
    );
    make_writable(repository_root.path());
}

#[test]
fn disabled_repository_record_stays_detached_after_restart() {
    let fixture = InstalledFixture::new("repository-disabled");
    let repository_root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    let mut repository = open_fixture_repository(repository_root.path(), Arc::clone(&trust));
    repository
        .install_archive(&fixture.archive, false)
        .expect("register disabled package");
    drop(repository);
    let restored = open_fixture_repository(repository_root.path(), trust);
    let system = test_plugin_system();

    restored
        .restore_into(&system)
        .expect("restore disabled registry");

    assert!(
        !system
            .is_installed("repository-disabled")
            .expect("runtime catalog")
    );
    make_writable(repository_root.path());
}

#[test]
fn tampered_content_addressed_archive_is_not_published() {
    let fixture = InstalledFixture::new("repository-tamper");
    let repository_root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    let mut repository = open_fixture_repository(repository_root.path(), Arc::clone(&trust));
    let record = repository
        .install_archive(&fixture.archive, true)
        .expect("register package");
    std::fs::OpenOptions::new()
        .append(true)
        .open(repository_root.path().join(&record.archive_path))
        .expect("open stored archive")
        .write_all(b"tamper")
        .expect("tamper archive");
    let system = test_plugin_system();

    let error = repository
        .restore_into(&system)
        .expect_err("tamper rejection");

    assert!(matches!(
        error,
        PluginRepositoryError::MetadataMismatch {
            field: "archive_digest",
            ..
        }
    ));
    assert!(
        !system
            .is_installed("repository-tamper")
            .expect("runtime catalog")
    );
    make_writable(repository_root.path());
}

#[test]
fn corrupt_registry_json_fails_open_without_state() {
    let fixture = InstalledFixture::new("repository-corrupt");
    let repository_root = tempfile::tempdir().expect("repository root");
    std::fs::write(repository_root.path().join("registry.json"), b"{not-json")
        .expect("write corrupt registry");

    let result = PluginRepository::open(
        repository_root.path(),
        fixture_trust(&fixture),
        HostCompatibility::new(u32::from(CURRENT_PROTOCOL_VERSION.major), TARGET),
    );
    let error = match result {
        Ok(_) => panic!("corrupt registry unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        PluginRepositoryError::InvalidRegistry { .. }
    ));
}

#[test]
fn failed_atomic_registry_write_keeps_previous_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = InstalledFixture::new("repository-atomic");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&fixture));
    repository
        .install_archive(&fixture.archive, true)
        .expect("register enabled package");
    std::fs::set_permissions(
        repository_root.path(),
        std::fs::Permissions::from_mode(0o555),
    )
    .expect("lock repository root");

    let result = repository.disable("repository-atomic");

    std::fs::set_permissions(
        repository_root.path(),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("unlock repository root");
    assert!(result.is_err());
    assert!(repository.registry().records[0].enabled);
    make_writable(repository_root.path());
}

#[test]
fn retain_and_purge_uninstall_apply_explicit_archive_policy() {
    let fixture = InstalledFixture::new("repository-uninstall");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&fixture));
    let record = repository
        .install_archive(&fixture.archive, false)
        .expect("register package");
    let archive = repository_root.path().join(&record.archive_path);

    repository
        .uninstall(
            "repository-uninstall",
            "1.0.0",
            UninstallPolicy::RetainArchive,
        )
        .expect("retain uninstall");
    assert!(archive.exists());
    repository
        .install_archive(&fixture.archive, false)
        .expect("register retained archive again");
    repository
        .uninstall("repository-uninstall", "1.0.0", UninstallPolicy::Purge)
        .expect("purge uninstall");
    assert!(!archive.exists());
    make_writable(repository_root.path());
}

#[test]
fn failed_uninstall_registry_commit_restores_staged_package_files() {
    let fixture = InstalledFixture::new("repository-uninstall-rollback");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&fixture));
    let record = repository
        .install_archive(&fixture.archive, false)
        .expect("register package");
    let archive = repository_root.path().join(&record.archive_path);
    let extracted = repository_root.path().join(&record.extracted_path);
    let registry_path = repository_root.path().join("registry.json");
    std::fs::remove_file(&registry_path).expect("remove registry file");
    std::fs::create_dir(&registry_path).expect("block registry persistence");

    repository
        .uninstall(
            "repository-uninstall-rollback",
            "1.0.0",
            UninstallPolicy::Purge,
        )
        .expect_err("registry commit failure");

    assert_eq!(repository.registry().records.len(), 1);
    assert!(archive.exists());
    assert!(extracted.exists());
    make_writable(repository_root.path());
}

#[test]
fn duplicate_version_is_rejected_without_registry_change() {
    let fixture = InstalledFixture::new("repository-duplicate");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&fixture));
    repository
        .install_archive(&fixture.archive, false)
        .expect("initial registration");

    let error = repository
        .install_archive(&fixture.archive, true)
        .expect_err("duplicate version rejection");

    assert!(matches!(
        error,
        PluginRepositoryError::DuplicateVersion { .. }
    ));
    assert_eq!(repository.registry().records.len(), 1);
    assert!(!repository.registry().records[0].enabled);
    make_writable(repository_root.path());
}

#[test]
fn registry_digest_mismatch_is_rejected_before_publication() {
    let fixture = InstalledFixture::new("repository-digest");
    let repository_root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    let mut repository = open_fixture_repository(repository_root.path(), Arc::clone(&trust));
    repository
        .install_archive(&fixture.archive, true)
        .expect("register package");
    let registry_path = repository_root.path().join("registry.json");
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).expect("read registry"))
            .expect("parse registry");
    snapshot["records"][0]["digest"] = json!("0".repeat(64));
    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&snapshot).expect("serialize registry"),
    )
    .expect("rewrite registry digest");
    let repository = open_fixture_repository(repository_root.path(), trust);
    let system = test_plugin_system();

    let error = repository
        .restore_into(&system)
        .expect_err("digest mismatch");

    assert!(matches!(
        error,
        PluginRepositoryError::MetadataMismatch {
            field: "digest",
            ..
        }
    ));
    assert!(
        !system
            .is_installed("repository-digest")
            .expect("runtime catalog")
    );
    make_writable(repository_root.path());
}

#[test]
fn one_tampered_enabled_archive_prevents_all_restore_publication() {
    let first = InstalledFixture::new("repository-first");
    let second = InstalledFixture::new("repository-second");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&first));
    repository
        .install_archive(&first.archive, true)
        .expect("register first package");
    let second_record = repository
        .install_archive(&second.archive, true)
        .expect("register second package");
    std::fs::OpenOptions::new()
        .append(true)
        .open(repository_root.path().join(second_record.archive_path))
        .expect("open second archive")
        .write_all(b"tamper")
        .expect("tamper second archive");
    let system = test_plugin_system();

    repository
        .restore_into(&system)
        .expect_err("transactional restore rejection");

    assert!(
        system
            .published_plugins()
            .expect("published catalog")
            .is_empty()
    );
    assert!(
        !system
            .is_installed("repository-first")
            .expect("runtime catalog")
    );
    make_writable(repository_root.path());
}

#[test]
fn partially_restore_enabled_plugins_disables_only_tampered_versions() {
    let first = InstalledFixture::new("repository-first");
    let second = InstalledFixture::new("repository-second");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&first));
    repository
        .install_archive(&first.archive, true)
        .expect("register first package");
    let second_record = repository
        .install_archive(&second.archive, true)
        .expect("register second package");
    std::fs::OpenOptions::new()
        .append(true)
        .open(repository_root.path().join(second_record.archive_path))
        .expect("open second archive")
        .write_all(b"tamper")
        .expect("tamper second archive");
    let system = test_plugin_system();
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&first));

    let report = repository
        .restore_with_degradation(&system)
        .expect("partial restore with degradation");

    assert_eq!(report.started, vec!["repository-first".to_string()]);
    assert_eq!(report.degraded.len(), 1);
    let degraded = report.degraded.first().expect("degraded plugin");
    assert_eq!(degraded.plugin_id, "repository-second");
    assert_eq!(
        degraded.kind,
        PluginRestoreFailureKind::PermanentAdmission,
        "tampered archive must classify as a permanent admission failure"
    );
    assert!(
        system
            .is_active("repository-first")
            .expect("active first plugin")
    );
    assert!(
        !system
            .is_active("repository-second")
            .expect("inactive second plugin")
    );
    assert!(
        !system
            .is_installed("repository-second")
            .expect("tampered record never reaches the runtime catalog"),
    );
    assert!(
        !repository
            .registry()
            .records
            .iter()
            .find(|record| record.plugin_id == "repository-second")
            .expect("second registry record")
            .enabled
    );
    make_writable(repository_root.path());
}

#[test]
fn partially_restore_notifies_each_started_plugin() {
    let first = InstalledFixture::new("repository-first");
    let second = InstalledFixture::new("repository-second");
    let repository_root = tempfile::tempdir().expect("repository root");
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&first));
    repository
        .install_archive(&first.archive, true)
        .expect("register first package");
    let second_record = repository
        .install_archive(&second.archive, true)
        .expect("register second package");
    std::fs::OpenOptions::new()
        .append(true)
        .open(repository_root.path().join(second_record.archive_path))
        .expect("open second archive")
        .write_all(b"tamper")
        .expect("tamper second archive");

    let system = test_plugin_system();
    let mut repository = open_fixture_repository(repository_root.path(), fixture_trust(&first));
    let mut notifications = Vec::new();

    let report = repository
        .restore_with_degradation_and_callback(&system, |plugin_id, version| {
            notifications.push((plugin_id.to_string(), version.to_string()));
        })
        .expect("partial restore with callback");

    notifications.sort();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].0, "repository-first");
    assert_eq!(report.started, vec!["repository-first".to_string()]);
    assert_eq!(report.degraded.len(), 1);
    assert_eq!(
        report.degraded.first().expect("degraded plugin").plugin_id,
        "repository-second"
    );
    assert!(
        system
            .is_active("repository-first")
            .expect("active first plugin")
    );
    assert!(
        !system
            .is_active("repository-second")
            .expect("inactive second plugin")
    );
    make_writable(repository_root.path());
}

#[test]
fn transient_handshake_timeout_remains_enabled_inactive_and_unpublished() {
    let fixture = InstalledFixture::new("handshake-timeout");
    let repository_root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    {
        let mut repository = open_fixture_repository(repository_root.path(), Arc::clone(&trust));
        repository
            .install_archive(&fixture.archive, true)
            .expect("register handshake-timeout package");
    }
    let mut config = fast_config();
    config.handshake_timeout = Duration::from_millis(100);
    let system = test_plugin_system_with_broker(config, Arc::new(DenyAllHostCapabilityBroker));
    let mut repository = open_fixture_repository(repository_root.path(), trust);

    let report = repository
        .restore_with_degradation(&system)
        .expect("restore with degradation");

    assert!(report.started.is_empty());
    assert_eq!(report.degraded.len(), 1);
    let failure = report.degraded.first().expect("degraded plugin");
    assert_eq!(failure.plugin_id, "handshake-timeout");
    assert_eq!(
        failure.kind,
        PluginRestoreFailureKind::TransientStartup,
        "process start failures must classify as transient so the supervisor retries"
    );
    assert!(
        system
            .is_installed("handshake-timeout")
            .expect("installed catalog"),
        "transient start failure should leave the plugin installed for retry"
    );
    assert!(
        !system
            .is_active("handshake-timeout")
            .expect("inactive after failed start"),
    );
    assert!(
        !system
            .published_plugins()
            .expect("published catalog")
            .iter()
            .any(|plugin| plugin.plugin_id == "handshake-timeout"),
        "transient failure must never publish the plugin"
    );
    assert!(
        repository
            .registry()
            .records
            .iter()
            .find(|record| record.plugin_id == "handshake-timeout")
            .expect("handshake-timeout record")
            .enabled,
        "transient start failure should leave the durable record enabled for retry"
    );

    // A subsequent supervisor-style start attempt also fails without flipping
    // durable enabled state.
    let error = system
        .start("handshake-timeout")
        .expect_err("second start still fails");
    assert!(
        matches!(error, PluginRuntimeError::HandshakeTimeout { .. }),
        "second start should time out the same way: {error:?}"
    );
    assert!(
        repository
            .registry()
            .records
            .iter()
            .find(|record| record.plugin_id == "handshake-timeout")
            .expect("handshake-timeout record")
            .enabled
    );

    make_writable(repository_root.path());
}

#[test]
fn static_schema_admission_failure_is_permanent_degradation() {
    let fixture = InstalledFixture::new("schema-compile-failure");
    let repository_root = tempfile::tempdir().expect("repository root");
    let trust = fixture_trust(&fixture);
    {
        let mut repository = open_fixture_repository(repository_root.path(), Arc::clone(&trust));
        repository
            .install_archive(&fixture.archive, true)
            .expect("register schema-compile-failure package");
    }
    let system = test_plugin_system();
    let mut repository = open_fixture_repository(repository_root.path(), trust);

    let report = repository
        .restore_with_degradation(&system)
        .expect("restore with degradation");

    assert!(report.started.is_empty());
    assert_eq!(report.degraded.len(), 1);
    let failure = report.degraded.first().expect("degraded plugin");
    assert_eq!(failure.plugin_id, "schema-compile-failure");
    assert_eq!(
        failure.kind,
        PluginRestoreFailureKind::PermanentAdmission,
        "static schema compilation must classify as a permanent admission failure"
    );
    assert!(
        !system
            .is_installed("schema-compile-failure")
            .expect("failed install never catalogs the plugin"),
    );
    assert!(
        !system
            .published_plugins()
            .expect("published catalog")
            .iter()
            .any(|plugin| plugin.plugin_id == "schema-compile-failure"),
    );
    assert!(
        !repository
            .registry()
            .records
            .iter()
            .find(|record| record.plugin_id == "schema-compile-failure")
            .expect("schema-compile-failure record")
            .enabled,
        "static admission failure should durably disable the record"
    );

    make_writable(repository_root.path());
}
