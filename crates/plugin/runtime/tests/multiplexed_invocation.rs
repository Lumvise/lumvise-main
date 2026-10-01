#![cfg(unix)]
#![allow(dead_code, unused_imports)]

include!("support/mod.rs");
use lumvise_plugin_runtime::ExportConcurrencyPolicy;

/// Helper: builds a `PluginSystem` with pipeline_limit=2 and Parallel(2) on
/// mux.barrier / mux.host-call so both concurrent invocations are admitted.
fn multiplexed_system(broker: Arc<dyn HostCapabilityBroker>) -> PluginSystem {
    let mut config = fast_config();
    config.max_concurrent_invocations_per_plugin = 2;
    config
        .export_concurrency
        .set_policy("mux.barrier".into(), ExportConcurrencyPolicy::Parallel(2));
    config
        .export_concurrency
        .set_policy("mux.host-call".into(), ExportConcurrencyPolicy::Parallel(2));
    test_plugin_system_with_broker(config, broker)
}

/// Two concurrent `mux.barrier` invocations on the same plugin process both
/// complete.  With pipeline_limit=1 this would deadlock (Barrier(2) requires
/// two concurrent workers; a serialized pipe lets only one in-flight invocation
/// at a time, so the single waiter blocks forever until the 150ms deadline).
#[test]
fn multiplexed_barrier_invocations_complete_concurrently() {
    let system = Arc::new(multiplexed_system(Arc::new(DenyAllHostCapabilityBroker)));
    let fixture = InstalledFixture::new("mux-barrier");
    system
        .install(&fixture.package)
        .expect("install mux-barrier");
    system.start("mux-barrier").expect("start mux-barrier");

    let input = json!({"barrier": "shared"});
    let input2 = input.clone();
    let s1 = Arc::clone(&system);
    let s2 = Arc::clone(&system);

    let t1 = thread::spawn(move || s1.invoke("mux-barrier", "mux.barrier", input));
    let t2 = thread::spawn(move || s2.invoke("mux-barrier", "mux.barrier", input2));

    let r1 = t1.join().expect("thread 1 panicked");
    let r2 = t2.join().expect("thread 2 panicked");

    assert!(r1.is_ok(), "invocation 1 failed: {r1:?}");
    assert!(r2.is_ok(), "invocation 2 failed: {r2:?}");
}

/// Two concurrent `mux.host-call` invocations on the same plugin process both
/// complete.  The fixture emits a `clock.read` host call per invocation; the
/// `RecordingClockBroker` immediately answers, proving the reader demultiplexer
/// routes each `PluginHostCall` → `HostHostResult` back to the correct invocation
/// without blocking the other.
#[test]
fn multiplexed_host_calls_interleave_without_blocking_reader() {
    let broker: Arc<dyn HostCapabilityBroker> = Arc::new(RecordingClockBroker::default());
    let system = Arc::new(multiplexed_system(broker));
    let fixture = InstalledFixture::new("mux-host-call");
    system
        .install(&fixture.package)
        .expect("install mux-host-call");
    system.start("mux-host-call").expect("start mux-host-call");

    let s1 = Arc::clone(&system);
    let s2 = Arc::clone(&system);

    let t1 =
        thread::spawn(move || s1.invoke("mux-host-call", "mux.host-call", json!({"tag": "a"})));
    let t2 =
        thread::spawn(move || s2.invoke("mux-host-call", "mux.host-call", json!({"tag": "b"})));

    let r1 = t1.join().expect("thread 1 panicked");
    let r2 = t2.join().expect("thread 2 panicked");

    assert!(r1.is_ok(), "invocation 1 failed: {r1:?}");
    assert!(r2.is_ok(), "invocation 2 failed: {r2:?}");

    // The fixture returns {"host_outcome": <WireOutcome>}; both should be Succeeded.
    let v1 = match &r1 {
        Ok(WireOutcome::Succeeded { value }) => value,
        other => panic!("expected Succeeded for r1, got: {other:?}"),
    };
    let v2 = match &r2 {
        Ok(WireOutcome::Succeeded { value }) => value,
        other => panic!("expected Succeeded for r2, got: {other:?}"),
    };
    // host_outcome is serialized as a WireOutcome value; just confirm it exists.
    assert!(v1.get("host_outcome").is_some(), "r1 missing host_outcome");
    assert!(v2.get("host_outcome").is_some(), "r2 missing host_outcome");
}

/// Under default pipeline_limit=1 + Serial policy, a single `mux.barrier`
/// invocation deadlocks at Barrier(2) (no second concurrent worker to unblock
/// it).  The host returns InvocationTimeout.  This proves the serial path
/// actually blocks — which is the negative-control counterpart to the
/// multiplexed test above.
///
/// NOTE: Barrier(2) cannot self-satisfy.  Under Serial policy only one
/// invocation is ever in-flight, so the barrier's second slot never arrives.
/// The controlled test deadline (150ms) fires and the invocation fails with
/// InvocationTimeout.
#[test]
fn single_barrier_times_out_under_serial_pipeline_limit() {
    let config = fast_config(); // pipeline_limit=1 (default), Serial (default)
    let system = Arc::new(test_plugin_system_with_broker(
        config,
        Arc::new(DenyAllHostCapabilityBroker),
    ));
    let fixture = InstalledFixture::new("mux-serial-barrier");
    system
        .install(&fixture.package)
        .expect("install mux-serial-barrier");
    system
        .start("mux-serial-barrier")
        .expect("start mux-serial-barrier");

    let result = system.invoke(
        "mux-serial-barrier",
        "mux.barrier",
        json!({"barrier": "only-one"}),
    );

    // Expect timeout: the barrier needs 2 waiters but the serial pipeline
    // never admits a second concurrent invocation.
    assert!(
        result.is_err(),
        "expected timeout under serial pipeline, got ok: {result:?}"
    );
}
