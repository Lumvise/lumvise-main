#[cfg(feature = "document-ml")]
use super::ConversionRuntime;
use super::{docling_export, require_content};
#[cfg(feature = "document-ml")]
use crate::documents::DocumentConversionOptions;
use docling::{DoclingDocument, Node, PictureImage};

fn pictured_page(bytes: Vec<u8>) -> DoclingDocument {
    let mut document = DoclingDocument::new("illustrated PDF");
    document.nodes = vec![
        Node::PageInfo {
            page_no: 2,
            width: 612.0,
            height: 792.0,
        },
        Node::Heading {
            level: 2,
            text: "Café heading".into(),
        },
        Node::Paragraph {
            text: "Original exact text.".into(),
        },
        Node::Located {
            location: [0, 0, 100, 100],
            inner: Box::new(Node::Picture {
                caption: Some("Original caption".into()),
                image: Some(PictureImage {
                    mimetype: "image/png".into(),
                    data: bytes,
                    width: 1,
                    height: 1,
                }),
                classification: None,
            }),
        },
    ];
    document
}

#[test]
fn export_records_exact_utf8_image_and_page_spans() {
    let converted = docling_export::export(pictured_page(vec![1, 2, 3]));
    assert!(
        converted
            .markdown
            .starts_with("# Page 2\n\n## Café heading")
    );
    let figure = &converted.provenance.figures[0];
    assert_eq!(figure.caption, "Original caption");
    assert_eq!(figure.page, Some(2));
    assert_eq!(
        &converted.markdown[figure.span.start..figure.span.end],
        format!("![Image]({})", figure.reference)
    );
    let page = &converted.provenance.pages[0];
    assert_eq!(page.page, 2);
    assert!(page.span.start <= figure.span.start && page.span.end >= figure.span.end);
    assert!(converted.markdown[page.span.start..page.span.end].contains("Original exact text."));
}

#[test]
fn changed_image_bytes_change_markdown_fingerprint() {
    let before = docling_export::export(pictured_page(vec![1, 2, 3]));
    let after = docling_export::export(pictured_page(vec![1, 2, 4]));
    assert_ne!(before.markdown, after.markdown);
    assert_eq!(before.images[0].bytes, vec![1, 2, 3]);
    assert!(!before.markdown.contains("base64"));
}

#[test]
fn empty_docling_output_is_a_descriptive_path_error() {
    let empty = docling_export::export(DoclingDocument::new("empty"));
    let error = require_content("empty.docx", empty).unwrap_err();
    assert_eq!(error.path, "empty.docx");
    assert!(error.reason.contains("expected nonempty"));
}

#[cfg(feature = "document-ml")]
#[test]
fn cached_initialization_failure_uses_basic_pdf_repeatedly() {
    let runtime = ConversionRuntime {
        pipeline: std::sync::Mutex::new(Some(Err("fake missing layout model".into()))),
    };
    for _ in 0..2 {
        let converted = runtime
            .convert(
                "report.pdf",
                include_bytes!("../../../tests/fixtures/documents/report.pdf"),
                DocumentConversionOptions::default(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(converted.provenance.enhancement, "unavailable");
        assert_eq!(converted.provenance.converter, "lopdf");
        assert!(
            converted
                .provenance
                .warnings
                .iter()
                .any(|warning| warning.contains("fake missing layout model"))
        );
    }
    assert!(runtime.pipeline.lock().unwrap().as_ref().unwrap().is_err());
}

#[cfg(feature = "document-ml")]
#[test]
fn missing_optional_assets_report_degradation_without_blocking_layout() {
    let entries =
        ["layout", "tableformer.decoder", "ocr.rec", "pdfium"].map(|stage| docling::ModelEntry {
            stage,
            path: format!("missing/{stage}"),
            found: false,
            bytes: 0,
        });
    let warnings = super::optional_model_warnings(entries.into());
    assert_eq!(warnings.len(), 2);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("tableformer.decoder"))
    );
    assert!(warnings.iter().any(|warning| warning.contains("ocr.rec")));
}

#[cfg(feature = "document-ml")]
struct FakePdfPipeline {
    outcome: Result<DoclingDocument, String>,
}

#[cfg(feature = "document-ml")]
impl super::PdfPipeline for FakePdfPipeline {
    fn convert(&mut self, _bytes: &[u8], _name: &str) -> Result<DoclingDocument, String> {
        self.outcome.clone()
    }
}

#[cfg(feature = "document-ml")]
fn fake_pdf_runtime(outcome: Result<DoclingDocument, String>) -> ConversionRuntime {
    ConversionRuntime {
        pipeline: std::sync::Mutex::new(Some(Ok(Box::new(FakePdfPipeline { outcome })))),
    }
}

#[cfg(feature = "document-ml")]
#[test]
fn inference_failure_and_empty_enhancement_retain_basic_pdf_text() {
    let outcomes = [
        Err("fake inference failure".into()),
        Ok(DoclingDocument::new("empty enhancement")),
    ];
    for outcome in outcomes {
        let converted = fake_pdf_runtime(outcome)
            .convert(
                "report.pdf",
                include_bytes!("../../../tests/fixtures/documents/report.pdf"),
                DocumentConversionOptions::default(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(converted.provenance.enhancement, "unavailable");
        assert_eq!(converted.provenance.pages.len(), 2);
        assert!(
            converted
                .markdown
                .contains("Coastal sensors require monthly calibration.")
        );
        assert!(!converted.provenance.warnings.is_empty());
    }
}

#[cfg(feature = "document-ml")]
#[test]
fn successful_enhancement_uses_primary_export_and_provenance() {
    let converted = fake_pdf_runtime(Ok(pictured_page(vec![1, 2, 3])))
        .convert(
            "report.pdf",
            b"supplied PDF bytes",
            DocumentConversionOptions::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(converted.provenance.converter, "docling");
    assert_eq!(converted.provenance.enhancement, "applied");
    assert_eq!(converted.provenance.figures[0].page, Some(2));
    assert!(converted.markdown.contains("Original exact text."));
}

#[test]
fn anytomd_images_keep_unicode_captions_exact_spans_and_content_digests() {
    let result = anytomd::ConversionResult {
        markdown: "## Slide 1: Café\n\n![Café chart](chart.png)\r\n".into(),
        images: vec![(
            "chart.png".into(),
            include_bytes!("../../../tests/fixtures/gradient.png").to_vec(),
        )],
        ..Default::default()
    };
    let converted = super::anytomd_export::export(result.clone());
    let figure = &converted.provenance.figures[0];
    assert_eq!(figure.caption, "Café chart");
    assert_eq!(
        &converted.markdown[figure.span.start..figure.span.end],
        format!("![Café chart]({})", figure.reference)
    );
    assert!(converted.markdown.ends_with("\r\n"));
    assert_eq!(converted.images[0].media_type, "image/png");
    let mut changed = result;
    changed.images[0].1.push(0);
    assert_ne!(
        converted.markdown,
        super::anytomd_export::export(changed).markdown
    );
}

#[test]
fn anytomd_unbacked_references_create_no_binary_or_figure_claim() {
    let result = anytomd::ConversionResult {
        markdown: "![External](https://example.invalid/image.png)\n".into(),
        ..Default::default()
    };
    let converted = super::anytomd_export::export(result);
    assert!(converted.images.is_empty());
    assert!(converted.provenance.figures.is_empty());
}
