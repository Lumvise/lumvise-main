use lumvise_project_indexer::{DocumentConversionOptions, DocumentConverter};

fn basic_converter() -> DocumentConverter {
    DocumentConverter::new(DocumentConversionOptions {
        enhancement_enabled: false,
    })
}

#[test]
fn illustrated_office_preserves_tables_and_binary_images() {
    let fixtures: &[(&str, &[u8])] = &[
        (
            "illustrated.docx",
            include_bytes!("fixtures/documents/illustrated.docx"),
        ),
        (
            "illustrated.xlsx",
            include_bytes!("fixtures/documents/illustrated.xlsx"),
        ),
        (
            "illustrated.pptx",
            include_bytes!("fixtures/documents/illustrated.pptx"),
        ),
    ];
    for (path, bytes) in fixtures {
        let converted = basic_converter().convert(path, bytes).unwrap().unwrap();
        assert!(
            converted.markdown.contains("Coast"),
            "{path}: {}",
            converted.markdown
        );
        assert!(converted.markdown.contains("|"), "{path}: missing table");
        assert!(!converted.images.is_empty(), "{path}: missing images");
        assert!(!converted.markdown.contains("base64"));
        assert_eq!(
            converted.provenance.converter,
            if path.ends_with(".pptx") {
                "anytomd"
            } else {
                "docling"
            }
        );
        assert_eq!(converted.provenance.enhancement, "not_applicable");
        for figure in &converted.provenance.figures {
            let image = converted
                .images
                .iter()
                .find(|image| image.reference == figure.reference)
                .unwrap();
            assert_eq!(image.media_type, "image/png");
            assert_eq!(image.bytes, include_bytes!("fixtures/gradient.png"));
            assert_eq!(
                &converted.markdown[figure.span.start..figure.span.end],
                format!(
                    "![{}]({})",
                    if path.ends_with(".pptx") {
                        figure.caption.as_str()
                    } else {
                        "Image"
                    },
                    figure.reference
                )
            );
        }
        assert!(!converted.provenance.figures.is_empty());
    }
}

#[test]
fn html_never_fetches_external_images_and_keeps_structure() {
    let html = b"<h1>Heading</h1><ul><li>One</li><li>Two</li></ul><img src='file:///etc/passwd'><img src='https://invalid.example/image.png'><table><tr><th>Key</th></tr><tr><td>Value</td></tr></table>";
    let converted = basic_converter()
        .convert("example.html", html)
        .unwrap()
        .unwrap();
    assert!(converted.markdown.contains("# Heading"));
    assert!(converted.markdown.contains("- One"));
    assert!(converted.markdown.contains("Value"));
    assert!(converted.images.is_empty());
    assert!(converted.provenance.figures.is_empty());
}

#[test]
fn rejects_malformed_empty_and_unsupported_inputs() {
    for (path, bytes) in [
        ("bad.docx", b"invalid ZIP".as_slice()),
        ("empty.html", b"".as_slice()),
        ("bad.pdf", b"not PDF".as_slice()),
    ] {
        let error = basic_converter().convert(path, bytes).unwrap_err();
        assert_eq!(error.path, path);
        assert!(!error.reason.is_empty());
    }
    assert!(
        basic_converter()
            .convert("native.rs", b"fn main() {}")
            .unwrap()
            .is_none()
    );
}

#[test]
fn disabled_pdf_retains_original_page_text() {
    let converted = basic_converter()
        .convert(
            "report.pdf",
            include_bytes!("fixtures/documents/report.pdf"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(converted.provenance.enhancement, "disabled");
    assert_eq!(converted.provenance.pages.len(), 2);
    let page = &converted.provenance.pages[1];
    assert_eq!(page.page, 2);
    assert!(
        converted.markdown[page.span.start..page.span.end]
            .contains("Coastal sensors require monthly calibration.")
    );
}

#[test]
fn uncaptained_picture_has_nonempty_deterministic_figure_name() {
    let converter = basic_converter();
    let bytes = include_bytes!("docling_adapter/no_caption.docx");
    let first = converter.convert("picture.docx", bytes).unwrap().unwrap();
    let second = converter.convert("picture.docx", bytes).unwrap().unwrap();
    assert_eq!(first.provenance.figures[0].caption, "Figure 1");
    assert_eq!(first.provenance.figures, second.provenance.figures);
}

#[test]
fn pptx_preserves_every_slide_heading_and_captioned_image() {
    let output = basic_converter()
        .convert(
            "illustrated.pptx",
            include_bytes!("fixtures/documents/illustrated.pptx"),
        )
        .unwrap()
        .unwrap();
    let headings: Vec<_> = output
        .markdown
        .lines()
        .filter(|line| line.starts_with("## Slide "))
        .collect();
    assert_eq!(
        headings,
        ["## Slide 1: Field report", "## Slide 2: Coastal chart"]
    );
    assert_eq!(output.provenance.converter, "anytomd");
    assert!(output.markdown.contains("Coast"));
    assert_eq!(
        output.images[0].bytes,
        include_bytes!("fixtures/gradient.png")
    );
    let figure = &output.provenance.figures[0];
    assert_eq!(figure.caption, "Coastal chart");
    assert_eq!(
        &output.markdown[figure.span.start..figure.span.end],
        format!("![Coastal chart]({})", figure.reference)
    );
    assert!(figure.reference.starts_with("images/"));
    assert!(!output.markdown.contains("base64"));
}

#[cfg(not(feature = "document-ml"))]
#[test]
fn default_converter_reports_uncompiled_enhancement() {
    let converted = DocumentConverter::default()
        .convert(
            "report.pdf",
            include_bytes!("fixtures/documents/report.pdf"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(converted.provenance.enhancement, "not_compiled");
    assert!(
        converted
            .provenance
            .warnings
            .iter()
            .any(|warning| warning.contains("not compiled"))
    );
}
