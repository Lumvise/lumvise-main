#![cfg(unix)]
#![expect(
    dead_code,
    unused_imports,
    reason = "shared integration support intentionally serves lifecycle and repository binaries"
)]

include!("support/mod.rs");

use std::os::unix::fs::PermissionsExt;

#[cfg(target_os = "macos")]
#[test]
fn production_system_invokes_real_plugin_inside_platform_sandbox() {
    let fixture = InstalledFixture::new("production-sandbox");
    let system = PluginSystem::production(fast_config(), Arc::new(DenyAllHostCapabilityBroker))
        .expect("current platform sandbox available");
    system.install(&fixture.package).expect("install package");

    system.start("production-sandbox").expect("sandboxed start");
    let outcome = system
        .invoke("production-sandbox", "echo.value", json!({"safe": true}))
        .expect("sandboxed invocation");

    assert_eq!(
        outcome,
        WireOutcome::Succeeded {
            value: json!({
                "capability_id": "echo.value",
                "input": {"safe": true}
            }),
        }
    );
    system.stop("production-sandbox").expect("sandboxed stop");
}

#[cfg(target_os = "macos")]
#[test]
fn production_system_rejects_executable_replaced_after_install() {
    let fixture = InstalledFixture::new("replaced-executable");
    let system = PluginSystem::production(fast_config(), Arc::new(DenyAllHostCapabilityBroker))
        .expect("current platform sandbox available");
    system.install(&fixture.package).expect("install package");
    replace_executable(fixture.package.executable());

    let error = system
        .start("replaced-executable")
        .expect_err("changed executable identity must fail closed");

    assert!(matches!(
        error,
        PluginRuntimeError::Sandbox { source, .. }
            if source.code == "executable_identity_mismatch"
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn production_system_denies_plugin_reads_outside_package() {
    let fixture = InstalledFixture::new("sandbox-read-denied");
    let system = PluginSystem::production(fast_config(), Arc::new(DenyAllHostCapabilityBroker))
        .expect("current platform sandbox available");
    system.install(&fixture.package).expect("install package");
    system
        .start("sandbox-read-denied")
        .expect("sandboxed start");
    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../Cargo.toml")
        .canonicalize()
        .expect("workspace manifest");

    let outcome = system
        .invoke(
            "sandbox-read-denied",
            "echo.value",
            json!({"path": workspace_manifest}),
        )
        .expect("denial is reported by fixture");

    assert_eq!(
        outcome,
        WireOutcome::Succeeded {
            value: json!({"read_error_kind": "permission_denied"}),
        }
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn production_sandbox_constructs_on_direct_launch_platform() {
    lumvise_plugin_runtime::ProductionPluginSandbox::new()
        .expect("direct-launch sandbox construction");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn production_sandbox_direct_launches_from_canonical_package_root() {
    let fixture = InstalledFixture::new("direct-launch");
    let sandbox = lumvise_plugin_runtime::ProductionPluginSandbox::new()
        .expect("direct-launch sandbox construction");
    let command = sandbox
        .prepare_command(PluginSandboxRequest {
            plugin_id: fixture.package.plugin_id(),
            package_digest: fixture.package.package_digest(),
            package_root: fixture.package.root(),
            executable: fixture.package.executable(),
            executable_sha256: fixture
                .package
                .file_sha256("bin/plugin-runtime-fixture")
                .expect("fixture executable identity"),
        })
        .expect("direct-launch command");

    assert_eq!(
        command.get_program(),
        fixture.package.executable().as_os_str()
    );
    let canonical_root = fixture
        .package
        .root()
        .canonicalize()
        .expect("canonical package root");
    assert_eq!(command.get_current_dir(), Some(canonical_root.as_path()));
}

fn replace_executable(executable: &Path) {
    let parent = executable.parent().expect("executable parent");
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755))
        .expect("make executable parent writable");
    let replacement = parent.join("replacement");
    std::fs::write(&replacement, b"not the signed executable").expect("write replacement");
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o555))
        .expect("make replacement executable");
    std::fs::rename(replacement, executable).expect("replace executable path");
}
