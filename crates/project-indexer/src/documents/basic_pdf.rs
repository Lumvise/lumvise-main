//! Dependable PDF fallback used when enhanced (Docling/ML) conversion is
//! unavailable or disabled. Extracts original page text and page anchors with
//! `lopdf`, plus embedded raster images on a best-effort basis. Text
//! extraction never interprets graphical paths — valid illustrated PDFs can
//! contain clipping paths that crash graphics-aware extractors — so image
//! discovery is a fully separate pass over `/XObject` dictionaries, not a
//! content-stream interpreter.
use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, RgbImage};
use lopdf::xobject::PdfImage;
use sha2::{Digest, Sha256};

use super::{ConvertedDocument, DocumentFigure, DocumentImage, DocumentPage, DocumentProvenance};
use crate::source::invalid;
use crate::{ScanError, SourceSpan};

/// Converts PDF bytes to Markdown with page anchors and best-effort embedded
/// images.
pub(super) fn convert(path: &str, bytes: &[u8]) -> Result<ConvertedDocument, ScanError> {
    let document = lopdf::Document::load_mem(bytes)
        .map_err(|error| invalid(path, format!("expected readable PDF: {error}")))?;

    let mut output = PdfOutput::default();
    for (page, page_id) in document.get_pages() {
        extract_page(&document, path, page, page_id, &mut output)?;
    }

    if output.pages.is_empty() {
        return Err(invalid(
            path,
            "expected PDF text; scanned documents require OCR",
        ));
    }
    Ok(output.into_document())
}

/// Accumulates one page at a time toward `ConvertedDocument`: fields mirror
/// `ConvertedDocument`/`DocumentProvenance` directly, so no separate
/// intermediate shape is needed before `into_document`.
#[derive(Default)]
struct PdfOutput {
    markdown: String,
    pages: Vec<DocumentPage>,
    figures: Vec<DocumentFigure>,
    images: Vec<DocumentImage>,
    warnings: Vec<String>,
    figure_count: usize,
}

impl PdfOutput {
    /// Appends a page's non-empty text as `# Page N` plus body, recording
    /// the Markdown span the page occupies. Empty pages add no heading.
    fn append_page_text(&mut self, page: u32, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.markdown.push_str(&format!("# Page {page}\n\n"));
        let start = self.markdown.len();
        self.markdown.push_str(text);
        let end = self.markdown.len();
        self.markdown.push_str("\n\n");
        self.pages.push(DocumentPage {
            page,
            span: SourceSpan { start, end },
        });
    }

    /// Appends `![caption](reference)` — basic mode has no layout, so this
    /// is always placed at the current end of the page's content — and
    /// records its exact Markdown span (excluding the trailing blank line).
    fn append_figure(&mut self, reference: &str, index: usize, page: u32) {
        let caption = format!("Figure {index} on page {page}");
        let start = self.markdown.len();
        self.markdown
            .push_str(&format!("![{caption}]({reference})"));
        let end = self.markdown.len();
        self.markdown.push_str("\n\n");
        self.figures.push(DocumentFigure {
            reference: reference.to_string(),
            caption,
            span: SourceSpan { start, end },
            page: Some(page),
        });
    }

    fn into_document(self) -> ConvertedDocument {
        ConvertedDocument {
            markdown: self.markdown,
            provenance: DocumentProvenance {
                converter: "lopdf",
                pages: self.pages,
                figures: self.figures,
                enhancement: "not_applicable",
                warnings: self.warnings,
            },
            images: self.images,
        }
    }
}

/// Extracts one page's text (always) and embeds its images (best effort).
fn extract_page(
    document: &lopdf::Document,
    path: &str,
    page: u32,
    page_id: lopdf::ObjectId,
    output: &mut PdfOutput,
) -> Result<(), ScanError> {
    // Text extraction must not interpret graphical paths: valid illustrated
    // PDFs can contain clipping paths that crash graphics-aware extractors.
    let text = document.extract_text(&[page]).map_err(|error| {
        invalid(
            path,
            format!("expected extractable PDF page {page}: {error}"),
        )
    })?;
    output.append_page_text(page, &text);
    embed_page_images(document, page, page_id, output);
    Ok(())
}

/// Embeds every raster XObject on a page. An unreadable resource dictionary
/// degrades to a warning rather than failing the page's text.
fn embed_page_images(
    document: &lopdf::Document,
    page: u32,
    page_id: lopdf::ObjectId,
    output: &mut PdfOutput,
) {
    let page_images = match document.get_page_images(page_id) {
        Ok(found) => found,
        Err(error) => {
            output.warnings.push(format!(
                "page {page}: failed to read embedded images: {error}"
            ));
            return;
        }
    };
    for pdf_image in page_images {
        output.figure_count += 1;
        embed_one_image(page, output.figure_count, &pdf_image, output);
    }
}

/// Encodes and records a single embedded image, or records why it was
/// skipped — never failing the surrounding page's text.
fn embed_one_image(page: u32, index: usize, pdf_image: &PdfImage<'_>, output: &mut PdfOutput) {
    match encode_pdf_image(pdf_image) {
        Ok((media_type, bytes)) => {
            let reference = reference_name(&bytes, &media_type);
            output.append_figure(&reference, index, page);
            output.images.push(DocumentImage {
                reference,
                media_type,
                bytes,
            });
        }
        Err(reason) => {
            output.warnings.push(format!(
                "page {page} image {index}: {reason}; skipped embedding"
            ));
        }
    }
}

/// Derives a deterministic, content-addressed Markdown reference (full
/// SHA-256 of the final bytes) so image bytes never need inlining as base64.
fn reference_name(bytes: &[u8], media_type: &str) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let extension = if media_type == "image/jpeg" {
        "jpg"
    } else {
        "png"
    };
    format!("pdf-image-{hex}.{extension}")
}

/// Encodes one embedded raster XObject to `(media_type, bytes)`. `DCTDecode`
/// (JPEG) is preserved verbatim; other supported raw rasters are re-encoded
/// as PNG. Anything else is an `Err` reason, not a hard failure.
fn encode_pdf_image(pdf_image: &PdfImage<'_>) -> Result<(String, Vec<u8>), String> {
    let filters = pdf_image.filters.clone().unwrap_or_default();
    if let Some(result) = encode_jpeg(&filters, pdf_image.content) {
        return result;
    }
    reject_unsupported_filter(&filters)?;
    reject_image_mask(pdf_image.origin_dict)?;

    let raw = decompress_raw_samples(pdf_image)?;
    encode_raw_raster_as_png(pdf_image, raw)
}

/// `Some` when the stream is JPEG-encoded (bare `DCTDecode`): its content is
/// already a self-contained JPEG file and is kept byte-for-byte rather than
/// decoded and re-encoded. `None` means the filter chain is something else.
fn encode_jpeg(filters: &[String], content: &[u8]) -> Option<Result<(String, Vec<u8>), String>> {
    if !filters.iter().any(|filter| filter == "DCTDecode") {
        return None;
    }
    Some(if filters.len() == 1 {
        Ok(("image/jpeg".to_string(), content.to_vec()))
    } else {
        Err(format!(
            "unsupported filter chain {filters:?} around DCTDecode"
        ))
    })
}

/// Rejects filters this fallback cannot decode without a dedicated codec
/// (JPEG 2000, CCITT fax, JBIG2 — all out of scope for a basic fallback).
fn reject_unsupported_filter(filters: &[String]) -> Result<(), String> {
    let unsupported = filters.iter().any(|filter| {
        matches!(
            filter.as_str(),
            "JPXDecode" | "CCITTFaxDecode" | "JBIG2Decode"
        )
    });
    if unsupported {
        return Err(format!("unsupported image filter {filters:?}"));
    }
    Ok(())
}

/// Rejects stencil image masks (`/ImageMask true`): their 1-bit samples
/// paint through another shape's color, not a standalone raster.
fn reject_image_mask(origin_dict: &lopdf::Dictionary) -> Result<(), String> {
    let is_mask = origin_dict
        .get(b"ImageMask")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if is_mask {
        return Err("stencil image masks are not supported".to_string());
    }
    Ok(())
}

/// Decompresses the stream's raw sample bytes (Flate/LZW/ASCII85/none).
/// `DCTDecode` never reaches here — `encode_jpeg` handles it first.
fn decompress_raw_samples(pdf_image: &PdfImage<'_>) -> Result<Vec<u8>, String> {
    let stream = lopdf::Stream {
        dict: pdf_image.origin_dict.clone(),
        content: pdf_image.content.to_vec(),
        allows_compression: true,
        start_position: None,
    };
    stream
        .decompressed_content()
        .map_err(|error| format!("failed to decompress image stream: {error}"))
}

/// Validates a PDF `Width`/`Height` as a positive `u32`. These are signed
/// in the PDF object model; a zero, negative, or overflowing value cannot
/// address real pixels and must error with the offending value, never be
/// silently treated as a valid raster size.
fn positive_dimension(value: i64, label: &str) -> Result<u32, String> {
    u32::try_from(value)
        .ok()
        .filter(|&dimension| dimension > 0)
        .ok_or_else(|| format!("expected positive {label}, got {value}"))
}

/// Builds the pixel buffer and re-encodes it as PNG, once the stream is
/// known to be a supported raw raster.
fn encode_raw_raster_as_png(
    pdf_image: &PdfImage<'_>,
    raw: Vec<u8>,
) -> Result<(String, Vec<u8>), String> {
    let width = positive_dimension(pdf_image.width, "width")?;
    let height = positive_dimension(pdf_image.height, "height")?;
    let bits_per_component = pdf_image.bits_per_component.unwrap_or(8);
    let color_space = pdf_image.color_space.as_deref().unwrap_or("DeviceGray");

    let dynamic = raw_raster_image(color_space, bits_per_component, width, height, raw).ok_or_else(|| {
        format!(
            "unsupported raw image encoding (color space {color_space}, {bits_per_component} bits per component, {width}x{height})"
        )
    })?;
    let mut png = Vec::new();
    dynamic
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|error| format!("failed to encode PNG: {error}"))?;
    Ok(("image/png".to_string(), png))
}

/// Builds a pixel buffer for the raw raster encodings this fallback
/// supports: 8-bit `DeviceGray`/`DeviceRGB` samples, no further codec.
fn raw_raster_image(
    color_space: &str,
    bits_per_component: i64,
    width: u32,
    height: u32,
    raw: Vec<u8>,
) -> Option<DynamicImage> {
    match (color_space, bits_per_component) {
        ("DeviceGray", 8) => GrayImage::from_raw(width, height, raw).map(DynamicImage::ImageLuma8),
        ("DeviceRGB", 8) => RgbImage::from_raw(width, height, raw).map(DynamicImage::ImageRgb8),
        _ => None,
    }
}
