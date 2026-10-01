#![cfg(unix)]
#![allow(dead_code, unused_imports)]

include!("support/mod.rs");

use lumvise_plugin_runtime::ExportConcurrencyPolicy;

const SINGLE_SAMPLE_COUNT: usize = 16;
const PARALLEL_WORKERS: usize = 8;
const PERFORMANCE_PLUGIN_ID: &str = "mux-operation-performance";
const PERFORMANCE_EXPORT_ID: &str = "echo.value";

fn performance_system() -> PluginSystem {
    let mut config = fast_config();
    config.max_concurrent_invocations_per_plugin = 8;
    config.export_concurrency.set_policy(
        PERFORMANCE_EXPORT_ID.into(),
        ExportConcurrencyPolicy::Parallel(8),
    );
    test_plugin_system_with_broker(config, Arc::new(DenyAllHostCapabilityBroker))
}

fn assert_succeeded(result: Result<WireOutcome, PluginRuntimeError>, label: &str) {
    match result {
        Ok(WireOutcome::Succeeded { .. }) => {}
        other => panic!("{label} failed: {other:?}"),
    }
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2
    } else {
        values[middle]
    }
}

fn percentile_p95(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    let rank = (values.len() * 95).saturating_add(99) / 100;
    values[rank.saturating_sub(1)]
}

#[test]
#[ignore = "prints runtime latency evidence on demand"]
fn reports_echo_operation_latency() {
    let fixture = InstalledFixture::new(PERFORMANCE_PLUGIN_ID);
    let system = Arc::new(performance_system());
    system
        .install(&fixture.package)
        .expect("install performance fixture");
    system
        .start(PERFORMANCE_PLUGIN_ID)
        .expect("start performance fixture");

    for warmup in 0..2 {
        assert_succeeded(
            system.invoke(
                PERFORMANCE_PLUGIN_ID,
                PERFORMANCE_EXPORT_ID,
                json!({"value": "warmup", "iteration": warmup}),
            ),
            "warmup invocation",
        );
    }

    let mut single_latencies = Vec::with_capacity(SINGLE_SAMPLE_COUNT);
    for sample in 0..SINGLE_SAMPLE_COUNT {
        let started = Instant::now();
        let result = system.invoke(
            PERFORMANCE_PLUGIN_ID,
            PERFORMANCE_EXPORT_ID,
            json!({"value": "single", "iteration": sample}),
        );
        single_latencies.push(started.elapsed().as_micros());
        assert_succeeded(result, "single invocation");
    }
    let single_median_us = median(&mut single_latencies);

    let release = Arc::new(Barrier::new(PARALLEL_WORKERS + 1));
    let workers = (0..PARALLEL_WORKERS)
        .map(|worker| {
            let system = Arc::clone(&system);
            let release = Arc::clone(&release);
            thread::spawn(move || {
                release.wait();
                let started = Instant::now();
                let result = system.invoke(
                    PERFORMANCE_PLUGIN_ID,
                    PERFORMANCE_EXPORT_ID,
                    json!({"value": "parallel", "worker": worker}),
                );
                (worker, result, started.elapsed().as_micros())
            })
        })
        .collect::<Vec<_>>();

    let parallel_started = Instant::now();
    release.wait();
    let mut parallel_latencies = Vec::with_capacity(PARALLEL_WORKERS);
    for worker in workers {
        let (worker_id, result, latency) = worker.join().expect("parallel worker thread");
        assert_succeeded(result, &format!("parallel invocation {worker_id}"));
        parallel_latencies.push(latency);
    }
    let parallel_batch_us = parallel_started.elapsed().as_micros();
    let parallel_per_operation_p95_us = percentile_p95(&mut parallel_latencies);

    println!(
        "{{\"workload\":\"{PERFORMANCE_EXPORT_ID}\",\"samples\":{SINGLE_SAMPLE_COUNT},\"single_median_us\":{single_median_us},\"parallel_workers\":{PARALLEL_WORKERS},\"parallel_batch_us\":{parallel_batch_us},\"parallel_per_operation_p95_us\":{parallel_per_operation_p95_us}}}"
    );
}
