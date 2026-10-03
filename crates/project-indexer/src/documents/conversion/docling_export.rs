//! Preserves Docling block serialization while attaching binary assets and byte ranges.
use super::super::{
    ConvertedDocument, DocumentFigure, DocumentImage, DocumentPage, DocumentProvenance,
};
use super::image_assets::{picture_caption, store_image};
use crate::SourceSpan;
use docling::{DoclingDocument, ImageMode, Node};

pub(super) fn export(document: DoclingDocument) -> ConvertedDocument {
    let mut converted = empty_export();
    let (mut start, mut page) = (0, None);
    for (index, node) in document.nodes.iter().enumerate() {
        if let Node::PageInfo { page_no, .. } = node {
            append_batch(&mut converted, &document, start..index, page);
            page = u32::try_from(*page_no).ok();
            start = index + 1;
        }
    }
    append_batch(&mut converted, &document, start..document.nodes.len(), page);
    converted
}

fn empty_export() -> ConvertedDocument {
    ConvertedDocument {
        markdown: String::new(),
        images: Vec::new(),
        provenance: DocumentProvenance {
            converter: "docling",
            pages: Vec::new(),
            figures: Vec::new(),
            enhancement: "not_applicable",
            warnings: Vec::new(),
        },
    }
}

fn append_batch(
    converted: &mut ConvertedDocument,
    original: &DoclingDocument,
    range: std::ops::Range<usize>,
    page: Option<u32>,
) {
    if range.is_empty() {
        return;
    }
    let batch = page_document(original, range);
    let (mut markdown, artifacts) =
        batch.export_to_markdown_with_images(ImageMode::Referenced, "images");
    let pictures = visible_pictures(&batch.nodes, converted.provenance.figures.len() + 1);
    let figures = replace_references(
        &mut markdown,
        artifacts,
        &pictures,
        &mut converted.images,
        page,
    );
    append_markdown(converted, markdown, figures, page);
}

fn page_document(original: &DoclingDocument, range: std::ops::Range<usize>) -> DoclingDocument {
    DoclingDocument {
        name: original.name.clone(),
        nodes: original.nodes[range].to_vec(),
        strict_markdown: original.strict_markdown,
        compact_tables: original.compact_tables,
        links: original.links.clone(),
    }
}

fn append_markdown(
    converted: &mut ConvertedDocument,
    markdown: String,
    figures: Vec<DocumentFigure>,
    page: Option<u32>,
) {
    if markdown.trim().is_empty() {
        return;
    }
    if !converted.markdown.is_empty() {
        converted.markdown.push('\n');
    }
    if let Some(page) = page {
        converted.markdown.push_str(&format!("# Page {page}\n\n"));
    }
    let offset = converted.markdown.len();
    converted.markdown.push_str(&markdown);
    offset_figures(converted, figures, offset);
    append_page_range(converted, page, offset);
}

fn offset_figures(converted: &mut ConvertedDocument, figures: Vec<DocumentFigure>, offset: usize) {
    converted
        .provenance
        .figures
        .extend(figures.into_iter().map(|mut figure| {
            figure.span.start += offset;
            figure.span.end += offset;
            figure
        }));
}

fn append_page_range(converted: &mut ConvertedDocument, page: Option<u32>, start: usize) {
    if let Some(page) = page {
        converted.provenance.pages.push(DocumentPage {
            page,
            span: SourceSpan {
                start,
                end: converted.markdown.len(),
            },
        });
    }
}

fn visible_pictures(nodes: &[Node], first_figure: usize) -> Vec<(String, String)> {
    let mut pictures = Vec::new();
    for node in nodes {
        collect_picture(node, first_figure, &mut pictures);
    }
    pictures
}

fn collect_picture(node: &Node, first_figure: usize, pictures: &mut Vec<(String, String)>) {
    match node {
        Node::Picture {
            caption,
            image: Some(image),
            ..
        } => {
            let caption = picture_caption(caption.as_deref(), first_figure + pictures.len());
            pictures.push((caption, image.mimetype.clone()));
        }
        Node::Group { children, .. } => {
            pictures.extend(visible_pictures(children, first_figure + pictures.len()));
        }
        Node::Located { inner, .. } => collect_picture(inner, first_figure, pictures),
        _ => {}
    }
}

fn replace_references(
    markdown: &mut String,
    artifacts: Vec<(String, Vec<u8>)>,
    pictures: &[(String, String)],
    images: &mut Vec<DocumentImage>,
    page: Option<u32>,
) -> Vec<DocumentFigure> {
    let mut figures = Vec::new();
    let mut cursor = 0;
    for ((old_reference, bytes), (caption, media_type)) in artifacts.into_iter().zip(pictures) {
        let reference = store_image(images, bytes, media_type);
        if let Some(span) = rewrite_reference(markdown, &old_reference, &reference, cursor) {
            cursor = span.end;
            figures.push(DocumentFigure {
                reference,
                caption: caption.clone(),
                span,
                page,
            });
        }
    }
    figures
}

fn rewrite_reference(
    markdown: &mut String,
    old_reference: &str,
    reference: &str,
    cursor: usize,
) -> Option<SourceSpan> {
    let old_markup = format!("![Image]({old_reference})");
    let start = cursor + markdown[cursor..].find(&old_markup)?;
    let markup = format!("![Image]({reference})");
    markdown.replace_range(start..start + old_markup.len(), &markup);
    Some(SourceSpan {
        start,
        end: start + markup.len(),
    })
}
