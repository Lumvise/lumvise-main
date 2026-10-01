use lumvise_db_core::{
    LocalPersistence, RelationalOperation, RelationalPersistence, RelationalResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

const RECORD_COUNT: usize = 500;
const SAMPLE_COUNT: usize = 5;

#[test]
#[ignore = "run through scripts/benchmarks/database-reopen; benchmark evidence is not a correctness gate"]
fn records_sampled_database_reopen_and_read_latency() {
    let workspace = tempfile::tempdir().unwrap();
    let database_path = workspace.path().join("database/lumvise.db");
    seed_database(&database_path);
    let _warm_up = measure_reopen(&database_path);
    let mut samples = (0..SAMPLE_COUNT)
        .map(|_| measure_reopen(&database_path))
        .collect::<Vec<_>>();
    samples.sort();

    eprintln!(
        "database-reopen records={RECORD_COUNT} samples={SAMPLE_COUNT} min={:?} median={:?} max={:?}",
        samples[0],
        samples[SAMPLE_COUNT / 2],
        samples[SAMPLE_COUNT - 1]
    );
}

fn seed_database(database_path: &Path) {
    let persistence = LocalPersistence::open(database_path).unwrap();
    let control = InvocationControl::sixty_seconds();
    for index in 0..RECORD_COUNT {
        RelationalPersistence::execute(
            &persistence,
            RelationalOperation::SetPersistentSetting {
                scope: "benchmark".into(),
                key: format!("record-{index}"),
                value: json!({ "index": index }),
            },
            &control,
        )
        .unwrap();
    }
}

fn measure_reopen(database_path: &Path) -> Duration {
    let started = Instant::now();
    let persistence = LocalPersistence::open(database_path).unwrap();
    let control = InvocationControl::sixty_seconds();
    for index in 0..RECORD_COUNT {
        let result = RelationalPersistence::execute(
            &persistence,
            RelationalOperation::GetPersistentSetting {
                scope: "benchmark".into(),
                key: format!("record-{index}"),
            },
            &control,
        )
        .unwrap();
        assert!(matches!(
            result,
            RelationalResult::PersistentSetting(Some(record))
                if record.value["index"] == index
        ));
    }
    started.elapsed()
}
