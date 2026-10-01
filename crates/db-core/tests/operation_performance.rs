use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use lumvise_db_core::{
    LocalPersistence, RelationalOperation, RelationalPersistence, RelationalResult,
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

const SAMPLE_COUNT: usize = 5;
const WORKER_COUNT: usize = 8;
const WARMUP_SAMPLE: usize = 0;

#[test]
#[ignore = "performance evidence harness; run explicitly with --ignored --nocapture"]
fn operation_performance_samples() {
    let workspace = tempfile::tempdir().unwrap();
    let persistence = Arc::new(
        LocalPersistence::open(workspace.path().join("lumvise.db"))
            .expect("open performance database"),
    );

    let _ = measure_setting_single(&persistence, WARMUP_SAMPLE);
    let _ = measure_setting_parallel(&persistence, WARMUP_SAMPLE);
    let _ = measure_semantic_single(&persistence, WARMUP_SAMPLE);
    let _ = measure_semantic_parallel(&persistence, WARMUP_SAMPLE);

    for sample in 1..=SAMPLE_COUNT {
        print_sample(
            "persistent_sqlite_setting_single",
            sample,
            1,
            measure_setting_single(&persistence, sample),
        );
        print_sample(
            "persistent_sqlite_setting_parallel",
            sample,
            WORKER_COUNT,
            measure_setting_parallel(&persistence, sample),
        );
        print_sample(
            "semantic_element_single",
            sample,
            1,
            measure_semantic_single(&persistence, sample),
        );
        print_sample(
            "semantic_element_parallel",
            sample,
            WORKER_COUNT,
            measure_semantic_parallel(&persistence, sample),
        );
    }
}

fn measure_setting_single(persistence: &LocalPersistence, sample: usize) -> Duration {
    let scope = format!("operation-performance-single-{sample}");
    let key = format!("setting-{sample}");
    let value = json!({ "sample": sample, "worker": 0 });
    let control = InvocationControl::sixty_seconds();
    let started = Instant::now();
    RelationalPersistence::execute(
        persistence,
        RelationalOperation::SetPersistentSetting {
            scope: scope.clone(),
            key: key.clone(),
            value: value.clone(),
        },
        &control,
    )
    .unwrap();
    let loaded = RelationalPersistence::execute(
        persistence,
        RelationalOperation::GetPersistentSetting { scope, key },
        &control,
    )
    .unwrap();
    assert!(matches!(
        loaded,
        RelationalResult::PersistentSetting(Some(record)) if record.value == value
    ));
    started.elapsed()
}

fn measure_setting_parallel(persistence: &Arc<LocalPersistence>, sample: usize) -> Duration {
    measure_parallel(persistence, move |persistence, worker| {
        let scope = format!("operation-performance-parallel-{sample}");
        let key = format!("setting-{worker}");
        let value = json!({ "sample": sample, "worker": worker });
        let control = InvocationControl::sixty_seconds();
        RelationalPersistence::execute(
            persistence,
            RelationalOperation::SetPersistentSetting {
                scope: scope.clone(),
                key: key.clone(),
                value: value.clone(),
            },
            &control,
        )
        .unwrap();
        let loaded = RelationalPersistence::execute(
            persistence,
            RelationalOperation::GetPersistentSetting { scope, key },
            &control,
        )
        .unwrap();
        assert!(matches!(
            loaded,
            RelationalResult::PersistentSetting(Some(record)) if record.value == value
        ));
    })
}

fn measure_semantic_single(persistence: &LocalPersistence, sample: usize) -> Duration {
    let element = benchmark_element(sample, 0);
    let control = InvocationControl::sixty_seconds();
    let started = Instant::now();
    store_and_load_element(persistence, element, &control);
    started.elapsed()
}

fn measure_semantic_parallel(persistence: &Arc<LocalPersistence>, sample: usize) -> Duration {
    measure_parallel(persistence, move |persistence, worker| {
        let control = InvocationControl::sixty_seconds();
        store_and_load_element(persistence, benchmark_element(sample, worker), &control);
    })
}

fn measure_parallel(
    persistence: &Arc<LocalPersistence>,
    operation: impl Fn(&LocalPersistence, usize) + Send + Sync + 'static,
) -> Duration {
    let ready = Arc::new(Barrier::new(WORKER_COUNT + 1));
    let start = Arc::new(Barrier::new(WORKER_COUNT + 1));
    let operation = Arc::new(operation);
    let handles = (0..WORKER_COUNT)
        .map(|worker| {
            let persistence = Arc::clone(persistence);
            let ready = Arc::clone(&ready);
            let start = Arc::clone(&start);
            let operation = Arc::clone(&operation);
            thread::spawn(move || {
                ready.wait();
                start.wait();
                operation(&persistence, worker);
            })
        })
        .collect::<Vec<_>>();
    ready.wait();
    let started = Instant::now();
    start.wait();
    for handle in handles {
        handle.join().unwrap();
    }
    started.elapsed()
}

fn store_and_load_element(
    persistence: &LocalPersistence,
    element: SemanticElement,
    control: &InvocationControl,
) {
    let stored = SemanticPersistence::execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: element.project_root.clone(),
            elements: vec![element.clone()],
            relationships: Vec::new(),
        },
        control,
    )
    .unwrap();
    assert!(matches!(
        stored,
        SemanticResult::SyncStructure(report) if report.elements_upserted == 1
    ));
    let loaded = SemanticPersistence::execute(
        persistence,
        SemanticOperation::Element {
            semantic_element_id: element.semantic_element_id.clone(),
        },
        control,
    )
    .unwrap();
    assert!(matches!(loaded, SemanticResult::Element(Some(found)) if found == element));
}

fn benchmark_element(sample: usize, worker: usize) -> SemanticElement {
    let id = format!("operation-performance:{sample}:{worker}");
    let simhash = ((sample as u64) << 32) | worker as u64;
    SemanticElement {
        project_root: format!("/operation-performance/{sample}/{worker}"),
        semantic_element_id: id.clone(),
        semantic_source_id: "operation-performance".into(),
        path: format!("src/{sample}/{worker}.rs"),
        element_kind: "function".into(),
        name: id,
        parent_element_id: None,
        content_fingerprint: Some(format!(
            "fp1:{simhash:016x}:operation-performance-{sample}-{worker}"
        )),
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({ "sample": sample, "worker": worker }),
    }
}

fn print_sample(workload: &str, sample: usize, workers: usize, elapsed: Duration) {
    eprintln!(
        "{{\"workload\":\"{workload}\",\"sample\":{sample},\"workers\":{workers},\"elapsed_ns\":{}}}",
        elapsed.as_nanos()
    );
}
