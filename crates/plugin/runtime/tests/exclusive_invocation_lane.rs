#![cfg(unix)]
#![allow(dead_code, unused_imports)]

include!("support/mod.rs");

fn ready_lane_system(
    plugin_id: &str,
) -> (
    InstalledFixture,
    Arc<PluginSystem>,
    Arc<ExclusiveInvocationLanes>,
) {
    let fixture = InstalledFixture::new(plugin_id);
    let lanes = Arc::new(ExclusiveInvocationLanes::new());
    let system = Arc::new(test_plugin_system_with_lanes(Arc::clone(&lanes)));
    system
        .install(&fixture.package)
        .expect("install lane fixture");
    system.start(plugin_id).expect("start lane fixture");
    (fixture, system, lanes)
}

fn lane_input(session_id: &str) -> serde_json::Value {
    json!({"owner_id": "owner", "session_id": session_id, "queue": true,
        "replace": false, "timeout_ms": 500, "phase": "active"})
}

#[test]
fn signed_lane_fifo_waits_before_process_dispatch() {
    let (_fixture, system, lanes) = ready_lane_system("lane-fifo");
    system
        .invoke("lane-fifo", "lane.acquire", lane_input("first"))
        .unwrap();
    let waiter_system = Arc::clone(&system);
    let waiter = thread::spawn(move || {
        waiter_system.invoke("lane-fifo", "lane.acquire", lane_input("second"))
    });
    wait_for_queued(&lanes, "lane-fifo", 1);

    system
        .invoke(
            "lane-fifo",
            "lane.release",
            json!({
                "owner_id": "owner", "session_id": "first", "queue": true,
                "replace": false, "timeout_ms": 500, "phase": "done"
            }),
        )
        .expect("release first");

    assert!(waiter.join().unwrap().is_ok());
    assert_eq!(
        snapshot(&lanes, "lane-fifo").active_session_id.as_deref(),
        Some("second")
    );
}

#[test]
fn replacement_is_same_owner_and_plugin_scoped() {
    let first_fixture = InstalledFixture::new("lane-first");
    let second_fixture = InstalledFixture::new("lane-second");
    let lanes = Arc::new(ExclusiveInvocationLanes::new());
    let system = test_plugin_system_with_lanes(Arc::clone(&lanes));
    system
        .install(&first_fixture.package)
        .expect("install first");
    system
        .install(&second_fixture.package)
        .expect("install second");
    system.start("lane-first").expect("start first");
    system.start("lane-second").expect("start second");
    system
        .invoke("lane-first", "lane.acquire", lane_input("one"))
        .unwrap();
    let mut replacement = lane_input("two");
    replacement["replace"] = json!(true);
    system
        .invoke("lane-first", "lane.acquire", replacement)
        .unwrap();
    system
        .invoke("lane-second", "lane.acquire", lane_input("foreign"))
        .unwrap();

    assert_eq!(
        snapshot(&lanes, "lane-first").active_session_id.as_deref(),
        Some("two")
    );
    assert_eq!(
        snapshot(&lanes, "lane-second").active_session_id.as_deref(),
        Some("foreign")
    );
}

#[test]
fn signed_timeout_and_queue_quota_fail_closed() {
    let (_fixture, system, lanes) = ready_lane_system("lane-limits");
    system
        .invoke("lane-limits", "lane.acquire", lane_input("active"))
        .unwrap();
    let waiter_system = Arc::clone(&system);
    let waiter = thread::spawn(move || {
        waiter_system.invoke("lane-limits", "lane.acquire", lane_input("queued"))
    });
    wait_for_queued(&lanes, "lane-limits", 1);

    let quota = system.invoke("lane-limits", "lane.acquire", lane_input("overflow"));
    assert!(matches!(
        quota,
        Err(PluginRuntimeError::ExclusiveLaneDenied { .. })
    ));
    lanes.cancel("lane-limits", "session").unwrap();
    assert!(waiter.join().unwrap().is_ok());

    let mut too_long = lane_input("too-long");
    too_long["timeout_ms"] = json!(1_001);
    assert!(matches!(
        system.invoke("lane-limits", "lane.acquire", too_long),
        Err(PluginRuntimeError::ExclusiveLaneDenied { .. })
    ));
}

#[test]
fn stop_cancels_waiters_and_crash_releases_active_lease() {
    let (_fixture, system, lanes) = ready_lane_system("lane-lifecycle");
    system
        .invoke("lane-lifecycle", "lane.acquire", lane_input("active"))
        .unwrap();
    let waiter_system = Arc::clone(&system);
    let waiter = thread::spawn(move || {
        waiter_system.invoke("lane-lifecycle", "lane.acquire", lane_input("queued"))
    });
    wait_for_queued(&lanes, "lane-lifecycle", 1);
    system.stop("lane-lifecycle").expect("stop plugin");
    assert!(matches!(
        waiter.join().unwrap(),
        Err(PluginRuntimeError::ExclusiveLaneCancelled { .. })
    ));
    system
        .uninstall("lane-lifecycle")
        .expect("uninstall stopped plugin");

    let (_crash_fixture, crash_system, crash_lanes) = ready_lane_system("lane-crash");
    crash_system
        .invoke("lane-crash", "lane.acquire", lane_input("active"))
        .unwrap();
    assert!(
        crash_system
            .invoke("lane-crash", "fixture.crash", json!({}))
            .is_err()
    );
    assert!(
        snapshot(&crash_lanes, "lane-crash")
            .active_session_id
            .is_none()
    );
}

#[test]
fn signed_application_failure_releases_provisional_lease() {
    let (_fixture, system, lanes) = ready_lane_system("lane-application-failure");
    let mut input = lane_input("failed");
    input["force_fail"] = json!(true);

    let outcome = system
        .invoke("lane-application-failure", "lane.acquire", input)
        .expect("application failure is a wire outcome");

    assert!(matches!(outcome, WireOutcome::Failed { .. }));
    assert!(
        snapshot(&lanes, "lane-application-failure")
            .active_session_id
            .is_none()
    );
}

fn snapshot(lanes: &ExclusiveInvocationLanes, plugin_id: &str) -> ExclusiveLaneSnapshot {
    lanes.snapshot(plugin_id, "session", "owner", None).unwrap()
}

fn wait_for_queued(lanes: &ExclusiveInvocationLanes, plugin_id: &str, expected: usize) {
    for _ in 0..1_000 {
        if snapshot(lanes, plugin_id).queued == expected {
            return;
        }
        thread::yield_now();
    }
    panic!("plugin `{plugin_id}` did not reach queued count `{expected}`");
}
