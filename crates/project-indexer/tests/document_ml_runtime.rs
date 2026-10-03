//! Real-model regression for the optional enhanced PDF path: these tests
//! call the actual `DocumentConverter` against the pinned local Docling
//! ONNX/PDFium assets (see `docling_pdf::model_inventory`), so they only
//! compile under `document-ml` and are `#[ignore]`d by default. Run once
//! the pinned models are provisioned:
//! `cargo test -p lumvise-project-indexer --test document_ml_runtime --features document-ml -- --ignored`
#![cfg(feature = "document-ml")]

use lumvise_project_indexer::{
    BlockSettings, DocumentConversionOptions, DocumentConverter, ParallelTreeSitterProjectParser,
    ProjectFileParser, SourceParseInput, TreeSitterProjectParser,
};
use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

fn enhanced_converter() -> DocumentConverter {
    DocumentConverter::default() // enhancement_enabled: true by default.
}

fn disabled_converter() -> DocumentConverter {
    DocumentConverter::new(DocumentConversionOptions {
        enhancement_enabled: false,
    })
}

/// A native (real text layer) PDF through the enhanced pipeline: enhancement
/// is applied, and the original page text survives recognition/layout,
/// without pinning to any particular Markdown structure.
#[test]
#[ignore = "requires provisioned local Docling models and PDFium"]
fn native_text_pdf_keeps_original_text_through_the_enhanced_pipeline() {
    let bytes = include_bytes!("fixtures/documents/report.pdf");
    let converted = enhanced_converter()
        .convert("report.pdf", bytes)
        .unwrap()
        .unwrap();
    assert_eq!(converted.provenance.enhancement, "applied");
    assert_eq!(converted.provenance.converter, "docling");
    assert!(!converted.provenance.pages.is_empty());
    assert!(
        converted
            .markdown
            .contains("Ocean measurements increased steadily.")
    );
    assert!(
        converted
            .markdown
            .contains("Coastal sensors require monthly calibration.")
    );
}

/// A scanned (image-only, no text layer) PDF: the enhanced pipeline must
/// recover its known phrases through real OCR, while disabling enhancement
/// on the exact same bytes must fall back to the basic PDF path and report
/// that OCR is required instead of inventing text.
#[test]
#[ignore = "requires provisioned local Docling models and PDFium"]
fn scanned_pdf_is_read_through_real_ocr_and_errors_without_enhancement() {
    let bytes = include_bytes!("fixtures/documents/scanned_field_report.pdf");
    let converted = enhanced_converter()
        .convert("scanned_field_report.pdf", bytes)
        .unwrap()
        .unwrap();
    assert_eq!(converted.provenance.enhancement, "applied");
    assert_eq!(converted.provenance.converter, "docling");
    let recognized = converted.markdown.to_ascii_uppercase();
    for phrase in ["FIELD REPORT", "COASTAL OBSERVATIONS"] {
        assert!(recognized.contains(phrase), "{}", converted.markdown);
    }
    let error = disabled_converter()
        .convert("scanned_field_report.pdf", bytes)
        .unwrap_err();
    assert!(
        error.to_string().contains("scanned documents require OCR"),
        "{error}"
    );
}

/// The live deadlock: lopdf forked Rayon work from a syntax worker that held the
/// shared PDF pipeline while a native preview waited on that same pipeline.
#[test]
#[ignore = "requires provisioned local Docling models and PDFium"]
fn enhanced_pdfs_in_mixed_batches_finish_beside_native_preview() {
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let converter = Arc::new(enhanced_converter());
        let fixtures: Vec<(String, &[u8])> = (0..8)
            .flat_map(|index| {
                [
                    (
                        format!("sheets{index}.xlsx"),
                        &include_bytes!("fixtures/documents/many_sheets.xlsx")[..],
                    ),
                    (format!("code{index}.rs"), &b"fn item() { target(); }"[..]),
                ]
            })
            .chain([
                (
                    "report.pdf".into(),
                    &include_bytes!("fixtures/documents/report.pdf")[..],
                ),
                (
                    "scanned_field_report.pdf".into(),
                    &include_bytes!("fixtures/documents/scanned_field_report.pdf")[..],
                ),
            ])
            .collect();
        let inputs: Vec<_> = fixtures
            .iter()
            .map(|(path, bytes)| SourceParseInput { path, bytes })
            .collect();
        let mut parallel = ParallelTreeSitterProjectParser::new_with_document_converter(
            4,
            BlockSettings::default(),
            Arc::clone(&converter),
        )
        .unwrap();
        let preview = {
            let converter = Arc::clone(&converter);
            thread::spawn(move || {
                let bytes = include_bytes!("fixtures/documents/report.pdf");
                (0..2)
                    .map(|_| converter.convert("report.pdf", bytes).unwrap().unwrap())
                    .last()
                    .unwrap()
            })
        };
        let parsed = parallel.parse_batch(&inputs).unwrap();
        let expected = TreeSitterProjectParser::default()
            .with_document_converter(Arc::clone(&converter))
            .parse_batch(&inputs)
            .unwrap();
        assert_eq!(parsed, expected);
        let previewed = preview.join().unwrap();
        assert_eq!(previewed.provenance.enhancement, "applied");
        let _ = done.send(());
    });
    if let Err(mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(Duration::from_secs(300)) {
        panic!("enhanced document batch deadlocked in nested Rayon scheduling");
    }
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}
