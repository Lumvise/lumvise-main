#![cfg(windows)]
#![expect(
    dead_code,
    unused_imports,
    reason = "shared integration support also serves lifecycle and repository test crates"
)]

include!("support/mod.rs");

use lumvise_plugin_runtime::{
    PluginInvocationClass, PluginInvocationContext, PluginInvocationRequest,
};

struct StopWindowsPluginBeforeFixtureCleanup<'a> {
    system: &'a PluginSystem,
    plugin_id: &'a str,
}

impl Drop for StopWindowsPluginBeforeFixtureCleanup<'_> {
    fn drop(&mut self) {
        // Assertions must not leave Windows holding the installed executable open.
        let _ = self.system.stop(self.plugin_id);
    }
}

#[test]
fn production_windows_invokes_signed_extensionless_plugin_after_restart() {
    let fixture = InstalledFixture::new("echo");
    assert_eq!(fixture.package.executable().extension(), None);
    let system = windows_production_system();
    let _cleanup = StopWindowsPluginBeforeFixtureCleanup {
        system: &system,
        plugin_id: "echo",
    };
    system
        .install(&fixture.package)
        .expect("install signed plugin");
    for message in ["windows-first-start", "windows-restart"] {
        assert_windows_production_round_trip(&system, message);
    }
}

fn windows_production_system() -> PluginSystem {
    PluginSystem::production(
        PluginRuntimeConfig::default(),
        Arc::new(DenyAllHostCapabilityBroker),
    )
    .expect("Windows production runtime")
}

fn assert_windows_production_round_trip(system: &PluginSystem, message: &str) {
    system
        .start("echo")
        .expect("start extensionless signed plugin");
    assert_windows_process_count(system, 1);
    let outcome = invoke_windows_fixture(system, message);
    system.stop("echo").expect("stop before fixture cleanup");
    assert_windows_process_count(system, 0);
    assert_eq!(
        outcome,
        WireOutcome::Succeeded {
            value: json!({"capability_id": "echo.value", "input": {"message": message}}),
        }
    );
}

fn assert_windows_process_count(system: &PluginSystem, expected: usize) {
    let active = system.active_processes().expect("process snapshot");
    assert_eq!(active.len(), expected);
    if let Some(process) = active.first() {
        assert_eq!(process.plugin_id, "echo");
        assert_ne!(process.process_id, 0);
        assert_ne!(process.process_id, std::process::id());
    }
}

fn invoke_windows_fixture(system: &PluginSystem, message: &str) -> WireOutcome {
    system
        .invoke_controlled(PluginInvocationRequest::new(
            "echo",
            "echo.value",
            json!({"message": message}),
            PluginInvocationContext::new(
                message,
                "windows-production-test-owner",
                PluginInvocationClass::Foreground,
                Instant::now() + PLUGIN_INVOCATION_DEADLINE,
            ),
        ))
        .expect("production controlled invocation")
}

#[test]
fn windows_signed_fixture_cleanup_removes_readonly_package() {
    let fixture = InstalledFixture::new("windows-cleanup");
    let package_root = fixture.package.root().to_path_buf();
    let executable = fixture
        .package
        .executable()
        .metadata()
        .expect("installed executable");
    assert!(executable.permissions().readonly());
    drop(fixture);
    assert!(
        !package_root.exists(),
        "fixture cleanup retained {package_root:?}"
    );
}
