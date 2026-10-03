//! Preserves AnyToMD slide sections while normalizing its extracted image blocks.
use super::image_assets::{picture_caption, store_image};
use crate::SourceSpan;
use crate::documents::{ConvertedDocument, DocumentFigure, DocumentProvenance};
use std::collections::HashMap;

pub(super) fn export(result: anytomd::ConversionResult) -> ConvertedDocument {
    let warnings = result
        .warnings
        .into_iter()
        .map(|warning| warning.message)
        .collect();
    let mut converted = empty_export(warnings);
    let references = register_images(&mut converted, result.images);
    for line in result.markdown.split_inclusive('\n') {
        append_line(&mut converted, line, &references);
    }
    converted
}

fn empty_export(warnings: Vec<String>) -> ConvertedDocument {
    ConvertedDocument {
        markdown: String::new(),
        images: Vec::new(),
        provenance: DocumentProvenance {
            converter: "anytomd",
            pages: Vec::new(),
            figures: Vec::new(),
            enhancement: "not_applicable",
            warnings,
        },
    }
}

fn register_images(
    converted: &mut ConvertedDocument,
    images: Vec<(String, Vec<u8>)>,
) -> HashMap<String, String> {
    let mut references = HashMap::new();
    for (filename, bytes) in images {
        let media_type = image::guess_format(&bytes)
            .map_or("application/octet-stream", |format| format.to_mime_type());
        let reference = store_image(&mut converted.images, bytes, media_type);
        if let Some(previous) = references.insert(filename.clone(), reference.clone()) {
            if previous != reference {
                converted.provenance.warnings.push(format!("AnyToMD returned conflicting image payloads for `{filename}`; reference uses last payload"));
            }
        }
    }
    references
}

fn append_line(
    converted: &mut ConvertedDocument,
    line: &str,
    references: &HashMap<String, String>,
) {
    let markup = line.trim_end_matches(['\r', '\n']);
    let Some((caption, filename)) = image_block(markup) else {
        converted.markdown.push_str(line);
        return;
    };
    let Some(reference) = references.get(filename) else {
        converted.markdown.push_str(line);
        return;
    };
    append_figure_block(converted, line, markup.len(), caption, reference);
}

fn append_figure_block(
    converted: &mut ConvertedDocument,
    line: &str,
    markup_length: usize,
    caption: &str,
    reference: &str,
) {
    let start = converted.markdown.len();
    converted
        .markdown
        .push_str(&format!("![{caption}]({reference})"));
    let span = SourceSpan {
        start,
        end: converted.markdown.len(),
    };
    converted.markdown.push_str(&line[markup_length..]);
    converted.provenance.figures.push(DocumentFigure {
        reference: reference.into(),
        caption: picture_caption(Some(caption), converted.provenance.figures.len() + 1),
        span,
        page: None,
    });
}

fn image_block(markup: &str) -> Option<(&str, &str)> {
    // AnyToMD emits extracted pictures as standalone inline-image blocks.
    let image = markup.strip_prefix("![")?.strip_suffix(')')?;
    let (caption, filename) = image.rsplit_once("](")?;
    Some((caption, filename))
}
