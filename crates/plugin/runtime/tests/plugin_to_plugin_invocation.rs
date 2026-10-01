#[expect(
    dead_code,
    unused_imports,
    reason = "shared integration support also serves lifecycle and repository test crates"
)]
mod support;

use std::{
    sync::{Arc, Barrier, mpsc},
    thread,
    time::{Duration, Instant},
};

use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{
    DenyAllHostCapabilityBroker, PluginInvocationCancellationRequest, PluginInvocationClass,
    PluginInvocationContext, PluginInvocationFailureKind, PluginInvocationRequest, PluginSystem,
};
use serde_json::{Value, json};
use support::*;

fn ready_pair(
    caller_id: &str,
    target_id: &str,
    allow: bool,
) -> (PluginSystem, InstalledFixture, InstalledFixture) {
    let caller = InstalledFixture::new(caller_id);
    let target = InstalledFixture::new(target_id);
    let broker: Arc<dyn lumvise_plugin_runtime::HostCapabilityBroker> = if allow {
        Arc::new(AllowPluginInvokeBroker)
    } else {
        Arc::new(DenyAllHostCapabilityBroker)
    };
    let system = test_plugin_system_with_broker(fast_config(), broker);
    system.install(&caller.package).expect("install caller");
    system.install(&target.package).expect("install target");
    system.start(caller_id).expect("start caller");
    system.start(target_id).expect("start target");
    (system, caller, target)
}

fn request(plugin_id: &str, export_id: &str, input: Value) -> Value {
    json!({"plugin_id": plugin_id, "export_id": export_id, "input": input})
}

fn invoke_proxy(system: &PluginSystem, caller_id: &str, input: Value) -> Value {
    let outcome = system
        .invoke(caller_id, "proxy.invoke", input)
        .expect("caller transport remains available");
    match outcome {
        WireOutcome::Succeeded { value } => value,
        other => panic!("expected caller success, got {other:?}"),
    }
}

fn host_error_code(value: &Value) -> &str {
    value["host_outcome"]["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("missing host error code in {value:?}"))
}

#[test]
fn signed_authorized_caller_invokes_ready_public_mcp_export() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-caller", "target-ready", true);

    let value = invoke_proxy(
        &system,
        "plugin-invoke-caller",
        request("target-ready", "target.echo", json!({"message": "hello"})),
    );

    assert_eq!(
        value["host_outcome"]["value"]["output"]["input"]["message"],
        "hello"
    );
}

#[test]
fn undeclared_caller_receives_stable_denial() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-undeclared", "target-ready", true);

    let value = invoke_proxy(
        &system,
        "plugin-invoke-undeclared",
        request("target-ready", "target.echo", json!({"message": "x"})),
    );

    assert_eq!(host_error_code(&value), "host_capability_undeclared");
}

#[test]
fn policy_denied_caller_receives_stable_denial() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-denied", "target-ready", false);

    let value = invoke_proxy(
        &system,
        "plugin-invoke-denied",
        request("target-ready", "target.echo", json!({"message": "x"})),
    );

    assert_eq!(host_error_code(&value), "host_capability_denied");
}

#[test]
fn absent_target_receives_stable_failure() {
    let caller = InstalledFixture::new("plugin-invoke-absent");
    let system = test_plugin_system_with_broker(fast_config(), Arc::new(AllowPluginInvokeBroker));
    system.install(&caller.package).expect("install caller");
    system.start("plugin-invoke-absent").expect("start caller");

    let value = invoke_proxy(
        &system,
        "plugin-invoke-absent",
        request("missing", "target.echo", json!({"message": "x"})),
    );

    assert_eq!(host_error_code(&value), "plugin_invoke_target_absent");
}

#[test]
fn stopped_target_receives_not_ready_failure() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-stopped", "target-stopped", true);
    system.stop("target-stopped").expect("stop target");

    let value = invoke_proxy(
        &system,
        "plugin-invoke-stopped",
        request("target-stopped", "target.echo", json!({"message": "x"})),
    );

    assert_eq!(host_error_code(&value), "plugin_invoke_target_not_ready");
}

#[test]
fn missing_export_receives_stable_failure() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-missing", "target-ready", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-missing",
        request("target-ready", "missing", json!({})),
    );
    assert_eq!(host_error_code(&value), "plugin_invoke_export_absent");
}

#[test]
fn non_mcp_export_is_denied() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-surface", "target-ready", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-surface",
        request("target-ready", "fixture.echo", json!({})),
    );
    assert_eq!(
        host_error_code(&value),
        "plugin_invoke_export_surface_denied"
    );
}

#[test]
fn scoped_mcp_export_is_denied() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-scoped", "scoped-catalog", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-scoped",
        request("scoped-catalog", "fixture.assistant", json!({})),
    );
    assert_eq!(
        host_error_code(&value),
        "plugin_invoke_export_surface_denied"
    );
}

#[test]
fn malformed_target_input_receives_schema_failure() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-input", "target-ready", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-input",
        request("target-ready", "target.echo", json!({"message": 3})),
    );
    assert_eq!(
        host_error_code(&value),
        "plugin_invoke_target_input_invalid"
    );
}

#[test]
fn plugin_invoke_rejects_extra_request_fields() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-shape", "target-ready", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-shape",
        json!({
            "plugin_id": "target-ready",
            "export_id": "target.echo",
            "input": {"message": "x"},
            "admin": true
        }),
    );
    assert_eq!(host_error_code(&value), "plugin_invoke_invalid_input");
}

#[test]
fn caller_cannot_invoke_itself() {
    let caller = InstalledFixture::new("plugin-invoke-self");
    let system = test_plugin_system_with_broker(fast_config(), Arc::new(AllowPluginInvokeBroker));
    system.install(&caller.package).expect("install caller");
    system.start("plugin-invoke-self").expect("start caller");
    let value = invoke_proxy(
        &system,
        "plugin-invoke-self",
        request("plugin-invoke-self", "proxy.invoke", json!({})),
    );
    assert_eq!(host_error_code(&value), "plugin_invoke_self_denied");
}

#[test]
fn nested_cycle_is_rejected_without_deadlock() {
    let (system, _a, _b) = ready_pair("plugin-invoke-a", "plugin-invoke-b", true);
    let back_to_a = request("plugin-invoke-a", "proxy.invoke", json!({}));
    let value = invoke_proxy(
        &system,
        "plugin-invoke-a",
        request("plugin-invoke-b", "proxy.invoke", back_to_a),
    );
    assert_eq!(
        host_error_code(&value["host_outcome"]["value"]["output"]),
        "plugin_invoke_cycle_denied"
    );
}

#[test]
fn concurrent_cross_thread_cycle_is_rejected_without_lock_inversion() {
    let (system, _a, _b) = ready_pair(
        "plugin-invoke-concurrent-a",
        "plugin-invoke-concurrent-b",
        true,
    );
    let system = Arc::new(system);
    let start = Arc::new(Barrier::new(3));
    let (sender, receiver) = mpsc::channel();
    for (caller, target) in [
        ("plugin-invoke-concurrent-a", "plugin-invoke-concurrent-b"),
        ("plugin-invoke-concurrent-b", "plugin-invoke-concurrent-a"),
    ] {
        let system = Arc::clone(&system);
        let start = Arc::clone(&start);
        let sender = sender.clone();
        thread::spawn(move || {
            start.wait();
            let value = invoke_proxy(&system, caller, request(target, "proxy.invoke", json!({})));
            sender.send(value).expect("send invocation result");
        });
    }
    start.wait();
    let results = [
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("first result"),
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("second result"),
    ];

    assert!(format!("{results:?}").contains("plugin_invoke_cycle_denied"));
    assert!(
        system
            .is_active("plugin-invoke-concurrent-a")
            .expect("A state")
    );
    assert!(
        system
            .is_active("plugin-invoke-concurrent-b")
            .expect("B state")
    );
}

#[test]
fn target_application_failure_does_not_unpublish_caller_or_target() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-failed", "target-failed", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-failed",
        request("target-failed", "target.echo", json!({"message": "x"})),
    );
    assert_eq!(host_error_code(&value), "plugin_invoke_target_failed");
    assert_eq!(
        value["host_outcome"]["error"]["details"]["target_error"]["code"],
        "target_application_failed"
    );
    assert!(
        system
            .is_active("plugin-invoke-failed")
            .expect("caller state")
    );
    assert!(system.is_active("target-failed").expect("target state"));
}

#[test]
fn invalid_target_output_unpublishes_target_only() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-output", "target-output", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-output",
        request("target-output", "target.invalid-output", json!({})),
    );
    assert_eq!(
        host_error_code(&value),
        "plugin_invoke_target_output_invalid"
    );
    assert!(!system.is_active("target-output").expect("target state"));
}

#[test]
fn target_crash_is_mapped_and_unpublished() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-crash", "crash", true);
    let value = invoke_proxy(
        &system,
        "plugin-invoke-crash",
        request("crash", "target.echo", json!({"message": "x"})),
    );
    assert_eq!(host_error_code(&value), "plugin_invoke_target_crashed");
    assert!(!system.is_active("crash").expect("target state"));
}

#[test]
fn target_timeout_is_mapped_without_deadlock() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-timeout", "timeout", true);
    let error = system
        .invoke(
            "plugin-invoke-timeout",
            "proxy.invoke",
            request("timeout", "target.echo", json!({"message": "x"})),
        )
        .expect_err("one shared deadline terminates caller and nested target");
    assert!(matches!(
        error,
        lumvise_plugin_runtime::PluginRuntimeError::InvocationTimeout { .. }
    ));
}

#[test]
fn nested_deadline_is_bounded_by_one_top_level_budget() {
    const TOP_LEVEL_BUDGET: Duration = Duration::from_millis(150);
    const SHUTDOWN_GRACE: Duration = Duration::from_millis(10);
    const SCHEDULER_SLACK: Duration = Duration::from_millis(100);

    let caller = InstalledFixture::new("plugin-invoke-budget-a");
    let middle = InstalledFixture::new("plugin-invoke-budget-b");
    let target = InstalledFixture::new("timeout");
    let mut config = lumvise_plugin_runtime::PluginRuntimeConfig::default()
        .with_controlled_test_deadline(TOP_LEVEL_BUDGET);
    config.handshake_timeout = Duration::from_secs(10);
    config.shutdown_grace = SHUTDOWN_GRACE;
    let system = test_plugin_system_with_broker(config, Arc::new(AllowPluginInvokeBroker));
    for fixture in [&caller, &middle, &target] {
        system.install(&fixture.package).expect("install fixture");
    }
    for plugin_id in [
        "plugin-invoke-budget-a",
        "plugin-invoke-budget-b",
        "timeout",
    ] {
        system.start(plugin_id).expect("start fixture");
    }
    let nested = request("timeout", "target.echo", json!({"message": "x"}));
    let started = Instant::now();
    let error = system
        .invoke(
            "plugin-invoke-budget-a",
            "proxy.invoke",
            request("plugin-invoke-budget-b", "proxy.invoke", nested),
        )
        .expect_err("one shared nested deadline");

    let elapsed = started.elapsed();
    let maximum_elapsed = TOP_LEVEL_BUDGET + SHUTDOWN_GRACE * 3 + SCHEDULER_SLACK;
    assert!(
        elapsed < maximum_elapsed,
        "nested call exceeded one top-level budget plus bounded cleanup/scheduler overhead: {elapsed:?} >= {maximum_elapsed:?}"
    );
    assert!(matches!(
        error,
        lumvise_plugin_runtime::PluginRuntimeError::InvocationTimeout { .. }
    ));
}

#[test]
fn cancelling_top_level_invocation_cancels_nested_target_and_allows_restart() {
    let (system, _caller, _target) = ready_pair("plugin-invoke-cancel", "timeout", true);
    let system = Arc::new(system);
    let invoking = Arc::clone(&system);
    let invocation = thread::spawn(move || {
        let context = PluginInvocationContext::new(
            "nested-cancel-request",
            "nested-owner",
            PluginInvocationClass::Foreground,
            Instant::now() + Duration::from_secs(2),
        )
        .with_route_identity("nested-session", None);
        invoking.invoke_controlled(PluginInvocationRequest::new(
            "plugin-invoke-cancel",
            "proxy.invoke",
            request("timeout", "target.echo", json!({"message": "blocked"})),
            context,
        ))
    });
    let wait_until = Instant::now() + Duration::from_secs(1);
    while !system
        .invocation_admission_snapshot("timeout")
        .expect("target admission")
        .executing
    {
        assert!(Instant::now() < wait_until, "nested target did not execute");
        thread::yield_now();
    }

    assert!(
        system
            .cancel_controlled(&PluginInvocationCancellationRequest {
                plugin_id: Some("plugin-invoke-cancel".into()),
                request_id: "nested-cancel-request".into(),
                owner_id: "nested-owner".into(),
                session_id: Some("nested-session".into()),
                scope_id: None,
            })
            .unwrap()
    );
    let error = invocation
        .join()
        .expect("nested cancellation join")
        .expect_err("top-level invocation cancelled");
    assert_eq!(error.kind(), PluginInvocationFailureKind::Cancelled);
    let target_snapshot = system.invocation_admission_snapshot("timeout").unwrap();
    assert!(!target_snapshot.executing);
    assert_eq!(target_snapshot.queued, 0);
    if !system.is_active("timeout").unwrap() {
        system.start("timeout").expect("restart nested target");
    }
    system
        .invoke("timeout", "echo.value", json!({"after": "cancel"}))
        .expect("target succeeds after restart");
}
