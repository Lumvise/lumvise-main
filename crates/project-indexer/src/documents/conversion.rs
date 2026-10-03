//! Private Docling adapter; the document facade owns the public contract.
mod anytomd_export;
mod docling_export;
mod image_assets;

use super::{ConvertedDocument, DocumentConversionOptions};
use crate::ScanError;
use docling::{DocumentConverter, InputFormat, SourceDocument};
#[cfg(feature = "document-ml")]
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct ConversionRuntime {
    #[cfg(feature = "document-ml")]
    pipeline: Mutex<Option<Result<Box<dyn PdfPipeline>, String>>>,
}

#[cfg(feature = "document-ml")]
trait PdfPipeline: Send {
    fn convert(&mut self, bytes: &[u8], name: &str) -> Result<docling::DoclingDocument, String>;
}

#[cfg(feature = "document-ml")]
struct DoclingPdfPipeline(docling::Pipeline);

#[cfg(feature = "document-ml")]
impl PdfPipeline for DoclingPdfPipeline {
    fn convert(&mut self, bytes: &[u8], name: &str) -> Result<docling::DoclingDocument, String> {
        self.0
            .convert(bytes, None, name)
            .map_err(|error| error.to_string())
    }
}

impl ConversionRuntime {
    pub(super) fn convert(
        &self,
        path: &str,
        bytes: &[u8],
        options: DocumentConversionOptions,
    ) -> Result<Option<ConvertedDocument>, ScanError> {
        let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if !super::is_document_path(path) {
            return Ok(None);
        }
        if extension == "pdf" {
            return self.convert_pdf(path, bytes, options).map(Some);
        }
        let converted = if matches!(extension.as_str(), "pptx" | "ipynb" | "xml") {
            convert_anytomd(path, bytes, &extension)?
        } else {
            convert_declarative(path, bytes, &extension)?
        };
        require_content(path, converted).map(Some)
    }

    fn convert_pdf(
        &self,
        path: &str,
        bytes: &[u8],
        options: DocumentConversionOptions,
    ) -> Result<ConvertedDocument, ScanError> {
        let (enhancement, warning) = self.pdf_attempt(path, bytes, options);
        if let Ok(converted) = enhancement {
            return Ok(converted);
        }
        let mut converted = super::basic_pdf::convert(path, bytes).map_err(|mut error| {
            if let Some(warning) = &warning {
                error.reason.push_str(&format!("; {warning}"));
            }
            error
        })?;
        converted.provenance.enhancement = enhancement.unwrap_err();
        converted.provenance.warnings.extend(warning);
        require_content(path, converted)
    }

    fn pdf_attempt(
        &self,
        path: &str,
        bytes: &[u8],
        options: DocumentConversionOptions,
    ) -> (Result<ConvertedDocument, &'static str>, Option<String>) {
        if !options.enhancement_enabled {
            return (Err("disabled"), None);
        }
        self.compiled_pdf_attempt(path, bytes)
    }

    #[cfg(feature = "document-ml")]
    fn compiled_pdf_attempt(
        &self,
        path: &str,
        bytes: &[u8],
    ) -> (Result<ConvertedDocument, &'static str>, Option<String>) {
        match self.enhanced_pdf(path, bytes) {
            Ok(converted) => (Ok(converted), None),
            Err(reason) => (
                Err("unavailable"),
                Some(format!(
                    "PDF enhancement unavailable for `{path}`: {reason}; used basic conversion"
                )),
            ),
        }
    }

    #[cfg(not(feature = "document-ml"))]
    fn compiled_pdf_attempt(
        &self,
        _path: &str,
        _bytes: &[u8],
    ) -> (Result<ConvertedDocument, &'static str>, Option<String>) {
        (
            Err("not_compiled"),
            Some("PDF enhancement not compiled; used basic conversion".into()),
        )
    }

    #[cfg(feature = "document-ml")]
    fn enhanced_pdf(&self, path: &str, bytes: &[u8]) -> Result<ConvertedDocument, String> {
        let mut guard = self
            .pipeline
            .lock()
            .map_err(|error| format!("PDF pipeline lock poisoned: {error}"))?;
        let pipeline = guard
            .get_or_insert_with(initialize_pipeline)
            .as_mut()
            .map_err(|reason| reason.clone())?;
        let document = pipeline.convert(bytes, path)?;
        let mut converted = docling_export::export(document);
        converted.provenance.enhancement = "applied";
        converted
            .provenance
            .warnings
            .extend(missing_optional_models());
        require_content(path, converted).map_err(|error| error.to_string())
    }
}

#[cfg(feature = "document-ml")]
fn missing_optional_models() -> Vec<String> {
    optional_model_warnings(docling::model_inventory())
}

#[cfg(feature = "document-ml")]
fn optional_model_warnings(entries: Vec<docling::ModelEntry>) -> Vec<String> {
    entries.into_iter()
        .filter(|entry| !entry.found && (entry.stage.starts_with("tableformer.") || entry.stage.starts_with("ocr.")))
        .map(|entry| format!("Missing optional PDF stage `{}` asset `{}`; OCR may be unavailable or tables reconstructed geometrically", entry.stage, entry.path))
        .collect()
}

#[cfg(feature = "document-ml")]
fn initialize_pipeline() -> Result<Box<dyn PdfPipeline>, String> {
    let missing: Vec<_> = docling::model_inventory()
        .into_iter()
        .filter(|entry| !entry.found && entry.stage != "pdfium")
        .map(|entry| format!("{}: {}", entry.stage, entry.path))
        .collect();
    if missing.iter().any(|entry| entry.starts_with("layout:")) {
        return Err(format!(
            "missing layout model; expected usable local ONNX assets ({})",
            missing.join(", ")
        ));
    }
    let mut pipeline = docling::Pipeline::new().map_err(|error| error.to_string())?;
    pipeline.warm_up().map_err(|error| error.to_string())?;
    Ok(Box::new(DoclingPdfPipeline(pipeline)))
}

fn convert_declarative(
    path: &str,
    bytes: &[u8],
    extension: &str,
) -> Result<ConvertedDocument, ScanError> {
    let format = InputFormat::from_extension(extension)
        .ok_or_else(|| conversion_error(path, "expected a supported document extension"))?;
    let source = SourceDocument::from_bytes(path, format, bytes.to_vec());
    let result = DocumentConverter::new().convert(source).map_err(|error| {
        conversion_error(
            path,
            &format!("expected valid {extension} document: {error}"),
        )
    })?;
    let mut converted = docling_export::export(result.document);
    if result.status != docling::ConversionStatus::Success {
        converted
            .provenance
            .warnings
            .push(format!("Docling conversion status: {:?}", result.status));
    }
    Ok(converted)
}

fn convert_anytomd(
    path: &str,
    bytes: &[u8],
    extension: &str,
) -> Result<ConvertedDocument, ScanError> {
    let options = anytomd::ConversionOptions {
        extract_images: true,
        ..Default::default()
    };
    let result = anytomd::convert_bytes(bytes, extension, &options).map_err(|error| {
        conversion_error(
            path,
            &format!("expected valid {extension} document: {error}"),
        )
    })?;
    Ok(anytomd_export::export(result))
}

fn require_content(
    path: &str,
    converted: ConvertedDocument,
) -> Result<ConvertedDocument, ScanError> {
    if converted.markdown.trim().is_empty() {
        return Err(conversion_error(
            path,
            "empty conversion; expected nonempty document text or embedded images",
        ));
    }
    Ok(converted)
}

fn conversion_error(path: &str, reason: &str) -> ScanError {
    ScanError {
        path: path.into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
#[path = "conversion/tests.rs"]
mod tests;
