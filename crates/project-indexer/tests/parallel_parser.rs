use lumvise_project_indexer::{
    BlockSettings, DocumentConversionOptions, DocumentConverter, FilesystemProjectSource,
    ParallelTreeSitterProjectParser, ParsedFile, ProjectFileParser, ProjectIndexer, ScanError,
    ScanScope, SemanticIndexProjection, SourceParseInput, TreeSitterProjectParser,
};
use std::{
    fs,
    num::NonZeroUsize,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

const MANY_SHEETS: &[u8] = include_bytes!("fixtures/documents/many_sheets.xlsx");
const REPORT_DOCX: &[u8] = include_bytes!("fixtures/documents/report.docx");
const REPORT_PDF: &[u8] = include_bytes!("fixtures/documents/report.pdf");

#[test]
fn workers_preserve_sequential_results_and_reuse_a_bounded_number_of_engines() {
    let fixtures: Vec<_> = (0..64)
        .map(|index| {
            (
                format!("{index}.rs"),
                format!("fn item{index}() {{ target(); }}"),
            )
        })
        .collect();
    let inputs: Vec<_> = fixtures
        .iter()
        .map(|(path, text)| SourceParseInput {
            path,
            bytes: text.as_bytes(),
        })
        .collect();
    let mut serial = TreeSitterProjectParser::default();
    let expected = serial.parse_batch(&inputs).unwrap();
    let mut parallel = ParallelTreeSitterProjectParser::new(4).unwrap();
    assert_eq!(parallel.batch_size().get(), 32);
    for _ in 0..3 {
        assert_eq!(parallel.parse_batch(&inputs).unwrap(), expected);
    }
    let metrics = parallel.metrics().unwrap();
    assert_eq!(metrics.trees_parsed, 192);
    assert!((1..=4).contains(&metrics.languages_initialized));
    assert!(parallel.parse_batch(&[]).unwrap().is_empty());
    assert_eq!(
        parallel.parse("single.rs", b"fn last() {}").unwrap(),
        serial.parse("single.rs", b"fn last() {}").unwrap()
    );
    assert_eq!(parallel.metrics().unwrap().trees_parsed, 193);
    assert!(ParallelTreeSitterProjectParser::new(0).is_err());
    assert!(ParallelTreeSitterProjectParser::new(usize::MAX).is_err());
}

#[test]
fn mixed_language_batches_keep_recovered_binary_and_unsupported_results_in_order() {
    let inputs = [
        SourceParseInput {
            path: "sample.rs",
            bytes: b"fn rust() {}",
        },
        SourceParseInput {
            path: "sample.py",
            bytes: b"def python():\n    target()\n",
        },
        SourceParseInput {
            path: "sample.ts",
            bytes: b"function typed(): void { target(); }",
        },
        SourceParseInput {
            path: "sample.json",
            bytes: br#"{"key":true}"#,
        },
        SourceParseInput {
            path: "broken.rs",
            bytes: b"fn broken(",
        },
        SourceParseInput {
            path: "binary.rs",
            bytes: b"\xff\x00",
        },
        SourceParseInput {
            path: "plain.txt",
            bytes: b"text",
        },
    ];
    let mut serial = TreeSitterProjectParser::default();
    let mut parallel = ParallelTreeSitterProjectParser::new(4).unwrap();
    assert_eq!(
        parallel.parse_batch(&inputs).unwrap(),
        serial.parse_batch(&inputs).unwrap()
    );
    assert_eq!(parallel.metrics().unwrap().trees_parsed, 5);
}

#[test]
fn parallel_initial_and_incremental_scans_publish_identical_batches() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..65 {
        fs::write(
            root.path().join(format!("{index}.rs")),
            format!("fn item{index}() {{ target(); }}"),
        )
        .unwrap();
    }
    fs::write(root.path().join("target.rs"), "fn target() {}").unwrap();
    let source = FilesystemProjectSource::open(root.path()).unwrap();
    let mut first_projection =
        SemanticIndexProjection::new(source.root(), "test-provider").unwrap();
    let mut second_projection =
        SemanticIndexProjection::new(source.root(), "test-provider").unwrap();
    let mut sequential = ProjectIndexer::new(source, TreeSitterProjectParser::default());
    let mut parallel = ProjectIndexer::new(
        FilesystemProjectSource::open(root.path()).unwrap(),
        ParallelTreeSitterProjectParser::new(4).unwrap(),
    );
    for (scope, expected_reads) in [
        (ScanScope::Full, 66),
        (ScanScope::Full, 0),
        (ScanScope::Paths(vec!["target.rs".into()]), 1),
    ] {
        if expected_reads == 1 {
            fs::write(root.path().join("target.rs"), "fn renamed() {}").unwrap();
        }
        let first = sequential.prepare(scope.clone()).unwrap();
        let second = parallel.prepare(scope).unwrap();
        assert_eq!(first.metrics(), second.metrics());
        assert_eq!(second.metrics().files_read, expected_reads);
        if first.needs_publication() {
            assert_eq!(
                first_projection.project(&first).unwrap(),
                second_projection.project(&second).unwrap()
            );
        }
        sequential.commit(first).unwrap();
        parallel.commit(second).unwrap();
    }
}

struct FakeIncompleteBatchParser;
impl ProjectFileParser for FakeIncompleteBatchParser {
    fn parse(&mut self, _: &str, _: &[u8]) -> Result<ParsedFile, ScanError> {
        unreachable!("batch-only fixture")
    }
    fn batch_size(&self) -> NonZeroUsize {
        NonZeroUsize::new(2).unwrap()
    }
    fn parse_batch(&mut self, _: &[SourceParseInput<'_>]) -> Result<Vec<ParsedFile>, ScanError> {
        Ok(vec![])
    }
}

#[test]
fn incomplete_batch_results_fail_without_acknowledging_any_source_changes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), "fn a() {}").unwrap();
    fs::write(root.path().join("b.rs"), "fn b() {}").unwrap();
    let mut indexer = ProjectIndexer::new(
        FilesystemProjectSource::open(root.path()).unwrap(),
        FakeIncompleteBatchParser,
    );
    for _ in 0..2 {
        let error = indexer
            .prepare(ScanScope::Full)
            .err()
            .expect("expected incomplete parser output to fail");
        assert_eq!(error.path, "0");
        assert_eq!(error.reason, "expected 2 ordered parser results");
    }
}

/// Docling forks Rayon work per sheet; on a syntax worker that fork could steal a
/// sibling parse for the worker's own locked parser and never return.
#[test]
fn mixed_document_batches_finish_beside_a_shared_preview_converter() {
    within_deadline(|| {
        let converter = Arc::new(DocumentConverter::new(DocumentConversionOptions {
            enhancement_enabled: false,
        }));
        let fixtures: Vec<(String, Vec<u8>)> = (0..32)
            .flat_map(|index| {
                [
                    (format!("sheets{index}.xlsx"), MANY_SHEETS.to_vec()),
                    (
                        format!("code{index}.rs"),
                        format!("fn item{index}() {{ target(); }}").into_bytes(),
                    ),
                ]
            })
            .chain([
                ("report.pdf".into(), REPORT_PDF.to_vec()),
                ("report.docx".into(), REPORT_DOCX.to_vec()),
            ])
            .collect();
        let inputs: Vec<_> = fixtures
            .iter()
            .map(|(path, bytes)| SourceParseInput { path, bytes })
            .collect();
        let expected = TreeSitterProjectParser::default()
            .with_document_converter(Arc::clone(&converter))
            .parse_batch(&inputs)
            .unwrap();
        let mut parallel = ParallelTreeSitterProjectParser::new_with_document_converter(
            4,
            BlockSettings::default(),
            Arc::clone(&converter),
        )
        .unwrap();
        let preview = {
            let converter = Arc::clone(&converter);
            thread::spawn(move || {
                (0..8)
                    .map(|_| {
                        converter
                            .convert("report.pdf", REPORT_PDF)
                            .unwrap()
                            .unwrap()
                    })
                    .last()
                    .unwrap()
            })
        };
        for _ in 0..4 {
            assert_eq!(parallel.parse_batch(&inputs).unwrap(), expected);
        }
        assert!(
            preview
                .join()
                .unwrap()
                .markdown
                .contains("Ocean measurements")
        );
    });
}

/// Fails instead of hanging; a deadlocked worker is abandoned to process exit.
fn within_deadline(work: impl FnOnce() + Send + 'static) {
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        work();
        let _ = done.send(());
    });
    if let Err(mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(Duration::from_secs(90)) {
        panic!("document batch deadlocked in nested Rayon scheduling");
    }
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}
