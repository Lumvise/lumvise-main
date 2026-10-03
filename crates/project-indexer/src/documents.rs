//! Owns document conversion, image assets and provenance. Call `DocumentConverter`
//! for reusable conversion or `convert_document` for one file. Adapters stay private.
mod basic_pdf;
mod conversion;
mod types;

use crate::{ScanError, SourceSpan};
use serde_json::{Value, json};
pub use types::*;

/// Reuses optional PDF inference across imports and previews without global state.
/// Example: `DocumentConverter::default().convert("report.docx", bytes)?`.
pub struct DocumentConverter {
    options: DocumentConversionOptions,
    runtime: conversion::ConversionRuntime,
}

impl Default for DocumentConverter {
    fn default() -> Self {
        Self::new(DocumentConversionOptions::default())
    }
}

impl DocumentConverter {
    /// Configures conversion independently of embedding settings.
    /// Example: `DocumentConverter::new(DocumentConversionOptions { enhancement_enabled: false })`.
    pub fn new(options: DocumentConversionOptions) -> Self {
        Self {
            options,
            runtime: conversion::ConversionRuntime::default(),
        }
    }

    /// Returns the configuration used by this reusable converter.
    /// Example: rebuild an import session when `converter.options()` changes.
    pub fn options(&self) -> DocumentConversionOptions {
        self.options
    }

    /// Converts supplied bytes without writing assets or fetching external images.
    /// Example: `converter.convert("table.xlsx", bytes)?`.
    pub fn convert(
        &self,
        path: &str,
        bytes: &[u8],
    ) -> Result<Option<ConvertedDocument>, ScanError> {
        self.runtime.convert(path, bytes, self.options)
    }
}

/// Converts formats without a dedicated semantic grammar; never intercepts code or JSON.
/// Example: `convert_document("table.csv", b"name,value\na,1")`.
pub fn convert_document(path: &str, bytes: &[u8]) -> Result<Option<ConvertedDocument>, ScanError> {
    DocumentConverter::default().convert(path, bytes)
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

impl DocumentProvenance {
    pub(crate) fn metadata(&self) -> Value {
        json!({"status":"converted", "converter":self.converter, "media_type":"text/markdown", "coordinate_space":"converted_markdown", "enhancement":self.enhancement, "warnings":self.warnings})
    }

    pub(crate) fn file_metadata(&self) -> Value {
        let mut metadata = self.metadata();
        metadata["images"] = json!(
            self.figures
                .iter()
                .map(|figure| json!({
                    "reference":figure.reference, "caption":figure.caption, "page":figure.page,
                    "start_byte":figure.span.start, "end_byte":figure.span.end
                }))
                .collect::<Vec<_>>()
        );
        metadata
    }

    pub(crate) fn selector(&self, source: &str, span: SourceSpan) -> Option<Value> {
        if let Some(figure) = self.figures.iter().find(|figure| figure.span == span) {
            return Some(
                json!({"kind":"document_image", "reference":figure.reference, "page":figure.page}),
            );
        }
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
