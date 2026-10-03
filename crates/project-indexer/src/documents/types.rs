//! Public document values; conversion adapters remain private to `documents`.
use crate::SourceSpan;

/// Controls optional local PDF inference independently of semantic embeddings.
/// Example: `DocumentConversionOptions { enhancement_enabled: false }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentConversionOptions {
    /// Attempt layout, table and OCR inference; failures retain basic conversion.
    pub enhancement_enabled: bool,
}

impl Default for DocumentConversionOptions {
    fn default() -> Self {
        Self {
            enhancement_enabled: true,
        }
    }
}

/// An original PDF page's UTF-8 range in converted Markdown.
/// Example: `DocumentPage { page: 1, span: SourceSpan { start: 0, end: 20 } }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentPage {
    pub page: u32,
    pub span: SourceSpan,
}

/// A figure's source reference and placement, without copying binary content into the index.
/// Example: `figure.reference` resolves against `ConvertedDocument::images`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentFigure {
    pub reference: String,
    pub caption: String,
    pub span: SourceSpan,
    pub page: Option<u32>,
}

/// Extracted image bytes owned by the conversion result, never written implicitly.
/// Example: a preview serves `image.bytes` with `image.media_type`.
#[derive(Debug)]
pub struct DocumentImage {
    pub reference: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// Conversion provenance; all spans refer to converted Markdown.
/// Example: `converted.provenance.warnings` explains degraded PDF conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentProvenance {
    pub converter: &'static str,
    pub pages: Vec<DocumentPage>,
    pub figures: Vec<DocumentFigure>,
    pub enhancement: &'static str,
    pub warnings: Vec<String>,
}

/// Derived Markdown, source mapping and referenced image payloads.
/// Example: pass `converted.markdown` to the existing Markdown semantic parser.
#[derive(Debug)]
pub struct ConvertedDocument {
    pub markdown: String,
    pub provenance: DocumentProvenance,
    pub images: Vec<DocumentImage>,
}
