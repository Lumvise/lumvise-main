//! Owns document conversion and provenance. Call convert_document with stable source
//! bytes; adapters and PDF extraction details stay private. No filesystem or DB writes.
use crate::source::invalid;
use crate::{ScanError, SourceSpan};
use serde_json::{Value, json};

/// Page content range in converted Markdown, excluding its generated heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentPage {
    /// One-based original PDF page number.
    pub page: u32,
    /// UTF-8 range in ConvertedDocument::markdown.
    pub span: SourceSpan,
}

/// Provenance retained with parsed definitions; coordinates always refer to Markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentProvenance {
    /// Converter identity, e.g. anytomd or pdf-extract.
    pub converter: &'static str,
    /// Source PDF pages; empty for other formats.
    pub pages: Vec<DocumentPage>,
}

/// Derived Markdown and its mapping back to the original document.
#[derive(Debug)]
pub struct ConvertedDocument {
    /// Complete converted text; passed through the existing semantic parser.
    pub markdown: String,
    /// Conversion source mapping.
    pub provenance: DocumentProvenance,
}

/// Converts formats without a dedicated semantic grammar; never intercepts code or JSON.
/// Example: `convert_document("table.csv", b"name,value\na,1")`.
pub fn convert_document(path: &str, bytes: &[u8]) -> Result<Option<ConvertedDocument>, ScanError> {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if extension == "pdf" {
        return pdf_document(path, bytes).map(Some);
    }
    if !is_document_path(path) {
        return Ok(None);
    }
    let output = anytomd::convert_bytes(bytes, &extension, &anytomd::ConversionOptions::default())
        .map_err(|error| {
            invalid(
                path,
                format!("expected convertible {extension} document: {error}"),
            )
        })?;
    if output.markdown.trim().is_empty() {
        return Err(invalid(path, "expected non-empty converted Markdown"));
    }
    Ok(Some(ConvertedDocument {
        markdown: output.markdown,
        provenance: DocumentProvenance {
            converter: "anytomd",
            pages: vec![],
        },
    }))
}

/// Whether a path uses derived Markdown. Example: `is_document_path("slides.pptx")`.
pub fn is_document_path(path: &str) -> bool {
    matches!(
        path.rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "pdf" | "docx" | "pptx" | "xlsx" | "xls" | "html" | "htm" | "csv" | "ipynb" | "xml"
    )
}

fn pdf_document(path: &str, bytes: &[u8]) -> Result<ConvertedDocument, ScanError> {
    let document = lopdf::Document::load_mem(bytes)
        .map_err(|error| invalid(path, format!("expected readable PDF: {error}")))?;
    let mut output = ConvertedDocument {
        markdown: String::new(),
        provenance: DocumentProvenance {
            converter: "lopdf",
            pages: vec![],
        },
    };
    for page in document.get_pages().keys() {
        // Text extraction must not interpret graphical paths: valid illustrated
        // PDFs can contain clipping paths that crash graphics-aware extractors.
        let text = document.extract_text(&[*page]).map_err(|error| {
            invalid(
                path,
                format!("expected extractable PDF page {page}: {error}"),
            )
        })?;
        append_pdf_page(&mut output, *page, &text);
    }
    if output.provenance.pages.is_empty() {
        return Err(invalid(
            path,
            "expected PDF text; scanned documents require OCR",
        ));
    }
    Ok(output)
}

fn append_pdf_page(output: &mut ConvertedDocument, page: u32, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    output.markdown.push_str(&format!("# Page {page}\n\n"));
    let start = output.markdown.len();
    output.markdown.push_str(text);
    let end = output.markdown.len();
    output.markdown.push_str("\n\n");
    output.provenance.pages.push(DocumentPage {
        page,
        span: SourceSpan { start, end },
    });
}

impl DocumentProvenance {
    pub(crate) fn metadata(&self) -> Value {
        json!({"status":"converted", "converter":self.converter, "media_type":"text/markdown", "coordinate_space":"converted_markdown"})
    }

    pub(crate) fn selector(&self, source: &str, span: SourceSpan) -> Option<Value> {
        let page = self
            .pages
            .iter()
            .find(|page| span.start < page.span.end && span.end > page.span.start)?;
        let start = span.start.max(page.span.start);
        let end = span.end.min(page.span.end);
        let exact = source.get(start..end)?.trim();
        (!exact.is_empty()).then(|| json!({"kind":"pdf_text", "page":page.page, "exact":exact}))
    }
}

/// Extend heading declarations over their section body without changing native Markdown parsing.
pub(crate) fn extend_sections(source: &str, definitions: &mut [crate::IndexedDefinition]) {
    definitions.sort_by_key(|definition| definition.span.start);
    let starts: Vec<_> = definitions
        .iter()
        .map(|definition| (definition.span.start, heading_level(source, definition)))
        .collect();
    for (index, definition) in definitions.iter_mut().enumerate() {
        let end = starts[index + 1..]
            .iter()
            .find(|(_, level)| *level <= starts[index].1)
            .map_or(source.len(), |(start, _)| *start);
        definition.span.end = end;
        definition.end_line = source[..end].lines().count().max(definition.start_line);
    }
}

fn heading_level(source: &str, definition: &crate::IndexedDefinition) -> usize {
    let heading = &source[definition.span.start..definition.span.end];
    let level = heading.chars().take_while(|ch| *ch == '#').count();
    if level > 0 {
        return level;
    }
    if heading
        .lines()
        .last()
        .is_some_and(|line| line.trim_start().starts_with('='))
    {
        1
    } else {
        2
    }
}
