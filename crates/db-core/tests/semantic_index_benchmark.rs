use lumvise_db_core::{
    LocalPersistence, SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::time::{Duration, Instant};

const SAMPLE_COUNT: usize = 5;
const ELEMENT_COUNT: usize = 1_000;

#[test]
#[ignore = "run through scripts/benchmarks/semantic-index; benchmark evidence is not a correctness gate"]
fn records_sampled_semantic_snapshot_throughput() {
    let _warm_up = measure_sample("warm-up");
    let mut samples = (0..SAMPLE_COUNT)
        .map(|index| measure_sample(&format!("sample-{index}")))
        .collect::<Vec<_>>();
    samples.sort();

    eprintln!(
        "semantic-index elements={ELEMENT_COUNT} samples={SAMPLE_COUNT} min={:?} median={:?} max={:?}",
        samples[0],
        samples[SAMPLE_COUNT / 2],
        samples[SAMPLE_COUNT - 1]
    );
}

fn measure_sample(prefix: &str) -> Duration {
    let persistence = LocalPersistence::in_memory().unwrap();
    let elements = (0..ELEMENT_COUNT)
        .map(|index| element(prefix, index))
        .collect::<Vec<_>>();
    let started = Instant::now();
    let result = SemanticPersistence::execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/benchmark".into(),
            elements,
            relationships: Vec::new(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert!(matches!(
        result,
        SemanticResult::SyncStructure(report) if report.elements_upserted == ELEMENT_COUNT
    ));
    started.elapsed()
}

fn element(prefix: &str, index: usize) -> SemanticElement {
    SemanticElement {
        project_root: "/benchmark".into(),
        semantic_element_id: format!("{prefix}:{index}"),
        semantic_source_id: "benchmark".into(),
        path: format!("src/{index}.rs"),
        element_kind: "function".into(),
        name: format!("item_{index}"),
        parent_element_id: None,
        content_fingerprint: Some(format!("fp1:{index:016x}:{prefix}-{index}")),
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}
