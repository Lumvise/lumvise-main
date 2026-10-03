//! Content-addressed image payloads shared by the document format adapters.
use crate::documents::DocumentImage;
use sha2::{Digest, Sha256};

pub(super) fn store_image(
    images: &mut Vec<DocumentImage>,
    bytes: Vec<u8>,
    media_type: &str,
) -> String {
    let extension = image::ImageFormat::from_mime_type(media_type)
        .map_or("bin", |format| format.extensions_str()[0]);
    let reference = format!("images/{:x}.{extension}", Sha256::digest(&bytes));
    if !images.iter().any(|image| image.reference == reference) {
        images.push(DocumentImage {
            reference: reference.clone(),
            media_type: media_type.into(),
            bytes,
        });
    }
    reference
}

pub(super) fn picture_caption(caption: Option<&str>, number: usize) -> String {
    caption
        .filter(|caption| !caption.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Figure {number}"))
}
