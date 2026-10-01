use lumvise_project_indexer::{
    FilesystemProjectSource, ProjectIndexer, ScanScope, SemanticIndexProjection,
    TreeSitterProjectParser,
};
use std::{fs, time::Instant};

#[test]
fn real_parser_scan_updates_only_changed_content_and_retries_unpublished_symbols() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("caller.rs"), "fn caller() { target(); }").unwrap();
    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let initial = indexer.prepare(ScanScope::Full).unwrap();
    assert_eq!(initial.metrics().files_parsed, 2);
    assert_eq!(
        initial
            .changed_files()
            .map(|file| file.parsed.definitions.len())
            .sum::<usize>(),
        2
    );
    indexer.commit(initial).unwrap();
    let unchanged = indexer.prepare(ScanScope::Full).unwrap();
    assert!(!unchanged.needs_publication());
    assert_eq!(
        (
            unchanged.metrics().files_read,
            unchanged.metrics().files_parsed
        ),
        (0, 0)
    );
    fs::write(root.path().join("target.rs"), "fn renamed() {}").unwrap();
    let scope = ScanScope::Paths(vec!["target.rs".into()]);
    drop(indexer.prepare(scope.clone()).unwrap());
    let retry = indexer.prepare(scope.clone()).unwrap();
    assert_eq!(retry.metrics().files_parsed, 1);
    let file = retry.changed_files().next().unwrap();
    assert_eq!(file.parsed.definitions[0].name, "renamed");
    assert_eq!(file.parsed.source.as_deref(), Some("fn renamed() {}"));
    indexer.commit(retry).unwrap();
    fs::remove_file(root.path().join("target.rs")).unwrap();
    let removed = indexer.prepare(scope).unwrap();
    assert_eq!(removed.removed_paths().collect::<Vec<_>>(), ["target.rs"]);
    assert_eq!(removed.metrics().files_parsed, 0);
}

#[test]
#[ignore = "release benchmark with actual Rust syntax parsing; excludes MCP/database publication"]
fn syntax_work_for_ten_thousand_files_and_one_changed_path() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..10_000 {
        let text = format!(
            "pub struct Sample{index};\nimpl Sample{index} {{\n pub fn caller(&self) {{ crate::target(); }}\n}}\n"
        );
        fs::write(root.path().join(format!("{index}.rs")), text).unwrap();
    }
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut projection = SemanticIndexProjection::new(source.root(), "benchmark-provider").unwrap();
    let mut indexer = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    for (label, scope, expected_parses) in [
        ("initial", ScanScope::Full, 10_000),
        ("unchanged", ScanScope::Full, 0),
        ("one_change", ScanScope::Paths(vec!["42.rs".into()]), 1),
        ("fanout_add", ScanScope::Paths(vec!["target.rs".into()]), 1),
        (
            "fanout_remove",
            ScanScope::Paths(vec!["target.rs".into()]),
            0,
        ),
    ] {
        if label == "one_change" {
            fs::write(root.path().join("42.rs"), "fn changed() { target(); }").unwrap();
        }
        if label == "fanout_add" {
            fs::write(root.path().join("target.rs"), "pub fn target() {}").unwrap();
        }
        if label == "fanout_remove" {
            fs::remove_file(root.path().join("target.rs")).unwrap();
        }
        let started = Instant::now();
        let scan = indexer.prepare(scope).unwrap();
        let elapsed_us = started.elapsed().as_micros();
        let work = scan.metrics();
        let reference_files = work.reference_files_resolved;
        let definitions: usize = scan
            .changed_files()
            .map(|file| file.parsed.definitions.len())
            .sum();
        let references: usize = scan
            .changed_files()
            .map(|file| file.parsed.references.len())
            .sum();
        let projected_before = projection.metrics().files_projected;
        let projection_started = Instant::now();
        let batch = scan
            .needs_publication()
            .then(|| projection.project(&scan).unwrap());
        let projection_us = projection_started.elapsed().as_micros();
        let json_started = Instant::now();
        let payload = batch.map(|batch| serde_json::to_value(batch).unwrap());
        let encoded = payload.map(|payload| serde_json::to_vec(&payload).unwrap());
        let json_us = json_started.elapsed().as_micros();
        let payload_bytes = encoded.as_ref().map_or(0, Vec::len);
        eprintln!(
            "{}",
            serde_json::json!({"workload": "rust_10k_source_and_projection", "case": label,
            "scan_us": elapsed_us, "projection_us": projection_us, "json_us": json_us, "payload_bytes": payload_bytes,
            "reads": work.files_read, "parses": work.files_parsed, "definitions": definitions, "references": references,
            "reference_files": reference_files, "new_file_fragments": projection.metrics().files_projected - projected_before})
        );
        assert_eq!(work.files_parsed, expected_parses);
        if label == "initial" {
            assert_eq!(definitions, 20_000);
            assert_eq!(references, 20_000);
        }
        if label == "fanout_add" {
            assert_eq!(reference_files, 10_001);
        }
        if label == "fanout_remove" {
            assert_eq!(reference_files, 10_000);
        }
        indexer.commit(scan).unwrap();
    }
}
