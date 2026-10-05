//! Read-only release evidence against a caller-selected real repository.
#[path = "repository_scan/timing.rs"]
mod timing;
use lumvise_project_indexer::{
    FilesystemProjectSource, ParseStatus, ProjectIndexer, ScanScope, SemanticIndexProjection,
    SourceKind,
};
use std::{collections::BTreeMap, path::Path, time::Instant};

#[test]
#[ignore = "set LUMVISE_INDEX_BENCH_ROOT; read-only release benchmark excluding MCP/database writes"]
fn real_repository_scan_reports_coverage_and_warm_inventory() {
    let root = std::env::var("LUMVISE_INDEX_BENCH_ROOT")
        .expect("expected LUMVISE_INDEX_BENCH_ROOT naming a repository");
    let source = FilesystemProjectSource::open(&root).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "benchmark-provider").unwrap();
    let costs = timing::StageCosts::default();
    let workers = std::env::var("LUMVISE_INDEX_BENCH_WORKERS").map_or(1, |value| {
        value.parse().expect("expected integer worker count")
    });
    let mut indexer = ProjectIndexer::new(costs.source(source), costs.parser(workers));
    let started = Instant::now();
    let scan = indexer.prepare(ScanScope::Full).unwrap();
    let scan_us = started.elapsed().as_micros();
    let mut coverage = BTreeMap::<String, usize>::new();
    for file in scan
        .changed_files()
        .filter(|file| file.entry.kind == SourceKind::File)
    {
        let extension = Path::new(&file.entry.path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("[none]");
        let status = match &file.parsed.status {
            ParseStatus::Unsupported => "unsupported",
            ParseStatus::PlainText => "text",
            ParseStatus::Binary => "binary",
            ParseStatus::Minified => "minified",
            ParseStatus::Parsed {
                has_syntax_errors: true,
                ..
            } => "recovered",
            ParseStatus::Parsed { .. } => "parsed",
        };
        *coverage.entry(format!("{extension}:{status}")).or_default() += 1;
    }
    let projected = Instant::now();
    let batch = projection.project(&scan).unwrap();
    let projection_us = projected.elapsed().as_micros();
    eprintln!(
        "{}",
        serde_json::json!({"case": "real_repository_initial", "scan_us": scan_us, "workers": workers,
        "projection_us": projection_us, "files": scan.metrics().files_read,
        "bytes": scan.metrics().bytes_read, "elements": batch.semantic_elements.len(),
        "relationships": batch.semantic_relationships.len(), "coverage": coverage})
    );
    eprintln!("{}", costs.report(scan_us));
    drop(batch);
    indexer.commit(scan).unwrap();
    let started = Instant::now();
    let warm = indexer.prepare(ScanScope::Full).unwrap();
    eprintln!(
        "{}",
        serde_json::json!({"case": "real_repository_warm", "scan_us": started.elapsed().as_micros(),
        "files_read": warm.metrics().files_read, "files_parsed": warm.metrics().files_parsed,
        "publication_needed": warm.needs_publication()})
    );
    // A live repository may change during the benchmark; report those reads rather
    // than assuming an external editor left every file unchanged.
}
