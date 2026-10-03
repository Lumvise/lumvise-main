//! Exercises the basic (lopdf) PDF fallback end to end through the public
//! `DocumentConverter` API. Every call here goes through `basic_converter()`,
//! which sets `enhancement_enabled: false` explicitly, so these tests take
//! the basic path deterministically regardless of whether `document-ml` is
//! compiled in or real Docling/PDFium models are provisioned locally — they
//! must not accidentally start exercising enhanced conversion once models
//! become available.
use lumvise_project_indexer::{DocumentConversionOptions, DocumentConverter};

fn basic_converter() -> DocumentConverter {
    DocumentConverter::new(DocumentConversionOptions {
        enhancement_enabled: false,
    })
}

#[test]
fn text_and_embedded_image_keep_page_anchors_and_reference_the_image_by_hash() {
    let bytes = include_bytes!("fixtures/documents/basic_text_image.pdf");
    let converted = basic_converter()
        .convert("report.pdf", bytes)
        .expect("conversion should not error")
        .expect("pdf is a recognized document path");

    assert_eq!(converted.provenance.converter, "lopdf");
    assert_eq!(converted.provenance.enhancement, "disabled");
    // A clean fixture like this one must add no warnings of its own.
    assert!(
        converted.provenance.warnings.is_empty(),
        "{:?}",
        converted.provenance.warnings
    );

    assert!(converted.markdown.starts_with("# Page 1\n\n"));
    assert!(converted.markdown.contains("Field report"));
    assert!(converted.markdown.contains("Coastal observations."));
    assert!(converted.markdown.contains("Region Count"));
    assert!(converted.markdown.contains("Coast 12"));

    // Page anchor span must point at exactly the extracted page text.
    assert_eq!(converted.provenance.pages.len(), 1);
    let page = &converted.provenance.pages[0];
    assert_eq!(page.page, 1);
    let spanned = &converted.markdown[page.span.start..page.span.end];
    assert!(spanned.contains("Field report"));
    assert!(spanned.contains("Coast 12"));

    // Exactly one embedded image, referenced (not inlined) from Markdown.
    assert_eq!(converted.images.len(), 1);
    assert_eq!(converted.provenance.figures.len(), 1);
    let image = &converted.images[0];
    let figure = &converted.provenance.figures[0];
    assert_eq!(image.media_type, "image/png");
    assert_eq!(figure.reference, image.reference);
    assert_eq!(figure.page, Some(1));
    assert!(
        !converted.markdown.contains("base64"),
        "image must not be inlined as base64"
    );
    let figure_markup = format!("![{}]({})", figure.caption, figure.reference);
    assert_eq!(
        &converted.markdown[figure.span.start..figure.span.end],
        figure_markup
    );
    assert!(converted.markdown.contains(&figure_markup));

    // The PNG bytes must decode back to the original 2x2 pixels.
    let decoded = image::load_from_memory(&image.bytes).expect("re-encoded PNG must decode");
    assert_eq!((decoded.width(), decoded.height()), (2, 2));

    // Deterministic reference: re-converting produces the identical reference/bytes.
    let converted_again = basic_converter()
        .convert("report.pdf", bytes)
        .unwrap()
        .unwrap();
    assert_eq!(converted_again.images[0].reference, image.reference);
    assert_eq!(converted_again.images[0].bytes, image.bytes);
}

#[test]
fn graphical_path_operators_never_break_or_leak_into_extracted_text() {
    // basic_text_image.pdf wraps its text in a clipping path (`W n`) and a
    // fill-color operator; a graphics-interpreting extractor is exactly what
    // this fallback must avoid becoming. Text must come through untouched.
    let bytes = include_bytes!("fixtures/documents/basic_text_image.pdf");
    let converted = basic_converter()
        .convert("illustrated.pdf", bytes)
        .unwrap()
        .unwrap();
    assert!(converted.markdown.contains("Field report"));
    assert!(
        !converted.markdown.contains("re W n"),
        "clipping-path operators must not leak into text"
    );
}

#[test]
fn jpeg_xobjects_are_preserved_verbatim_not_recompressed() {
    let bytes = include_bytes!("fixtures/documents/jpeg_embed.pdf");
    let original_jpeg = include_bytes!("fixtures/documents/tiny.jpg");
    let converted = basic_converter()
        .convert("chart.pdf", bytes)
        .unwrap()
        .unwrap();

    assert_eq!(converted.images.len(), 1);
    let image = &converted.images[0];
    assert_eq!(image.media_type, "image/jpeg");
    assert_eq!(&image.bytes, original_jpeg.as_slice());
    assert!(image.reference.ends_with(".jpg"));
    assert!(converted.markdown.contains(&image.reference));
}

#[test]
fn scanned_page_with_only_an_image_errors_as_ocr_required_not_fake_text() {
    let bytes = include_bytes!("fixtures/documents/image_only.pdf");
    let error = basic_converter()
        .convert("scan.pdf", bytes)
        .expect_err("a page with pixels but no extractable text must error");
    assert!(
        error.reason.contains("scanned documents require OCR"),
        "unexpected reason: {}",
        error.reason
    );
}

#[test]
fn unsupported_image_encoding_warns_but_does_not_break_text_extraction() {
    let bytes = include_bytes!("fixtures/documents/unsupported_image.pdf");
    let converted = basic_converter()
        .convert("mixed.pdf", bytes)
        .expect("text extraction must succeed despite the unsupported image")
        .unwrap();

    assert!(converted.markdown.contains("Field report"));
    assert!(converted.markdown.contains("Coastal observations."));
    assert!(
        converted.images.is_empty(),
        "unsupported image must not be embedded"
    );
    assert!(converted.provenance.figures.is_empty());
    assert!(
        converted
            .provenance
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported")),
        "expected an unsupported-image warning, got {:?}",
        converted.provenance.warnings
    );
}

#[test]
fn corrupt_pdf_bytes_error_with_path_and_expected_readable_pdf() {
    let error = basic_converter()
        .convert("broken.pdf", b"not a pdf")
        .expect_err("garbage bytes are not a valid PDF");
    assert_eq!(error.path, "broken.pdf");
    assert!(
        error.reason.contains("expected readable PDF"),
        "unexpected reason: {}",
        error.reason
    );
}

#[test]
fn existing_source_selector_page_text_regression_continues() {
    // The long-standing report.pdf fixture (used by document_conversion.rs)
    // must keep producing the same page-anchored Markdown through this path.
    let bytes = include_bytes!("fixtures/documents/report.pdf");
    let converted = basic_converter()
        .convert("report.pdf", bytes)
        .unwrap()
        .unwrap();
    assert_eq!(converted.provenance.pages.len(), 2);
    let second_page = &converted.provenance.pages[1];
    assert_eq!(second_page.page, 2);
    let exact = converted.markdown[second_page.span.start..second_page.span.end].trim();
    assert_eq!(exact, "Coastal sensors require monthly calibration.");
}
