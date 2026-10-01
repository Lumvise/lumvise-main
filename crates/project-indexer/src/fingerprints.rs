//! Source identity fingerprints preserve the existing fp1 format and algorithms.
//! Exact scan freshness uses SHA256 separately; these fingerprints serve semantic
//! rename/move matching. See the public golden fixtures before changing an algorithm.
mod waveform;

use std::path::Path;

/// Existing semantic matching fingerprint and its declared algorithm.
/// Example: use `file.fingerprint.as_ref().map(|value| &value.encoded)` for projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFingerprint {
    /// Portable `fp1:<simhash>:<exact FNV>` representation.
    pub encoded: String,
    /// Existing algorithm identifier, retained for matching consistency.
    pub algorithm: &'static str,
}

impl SourceFingerprint {
    /// Computes a declaration fingerprint from its trimmed source body.
    /// Example: `SourceFingerprint::text("fn example() {}")`.
    pub fn text(text: &str) -> Self {
        Self::encode(
            text_simhash(text),
            text.as_bytes(),
            "text-token-simhash-fp1-v1",
        )
    }

    /// Computes a file fingerprint from bytes already read by the scanner.
    /// Example: `SourceFingerprint::file("photo.png", bytes)`.
    pub fn file(path: &str, bytes: &[u8]) -> Self {
        match file_kind(path) {
            "image" => Self::media(
                bytes,
                image_dhash(bytes),
                "image-dhash-fp1-v1",
                "image-byte-window-fp1-v1",
            ),
            "audio" => Self::media(
                bytes,
                waveform::simhash(bytes),
                "audio-waveform-fp1-v1",
                "audio-byte-window-fp1-v1",
            ),
            "video" => Self::encode(media_simhash(bytes), bytes, "video-byte-window-fp1-v1"),
            _ => Self::ordinary_file(bytes),
        }
    }

    fn ordinary_file(bytes: &[u8]) -> Self {
        // Preserve the established text/byte algorithm boundary, independently of
        // whether a language adapter can extract structure from a larger file.
        if bytes.len() <= 1_000_000
            && let Ok(text) = std::str::from_utf8(bytes)
        {
            return Self::text(text);
        }
        Self::encode(byte_simhash(bytes), bytes, "file-byte-window-fp1-v1")
    }

    fn media(
        bytes: &[u8],
        decoded: Option<u64>,
        decoded_algorithm: &'static str,
        fallback: &'static str,
    ) -> Self {
        match decoded {
            Some(simhash) => Self::encode(simhash, bytes, decoded_algorithm),
            None => Self::encode(media_simhash(bytes), bytes, fallback),
        }
    }

    fn encode(simhash: u64, bytes: &[u8], algorithm: &'static str) -> Self {
        Self {
            encoded: format!("fp1:{simhash:016x}:{:016x}", stable_hash(bytes)),
            algorithm,
        }
    }
}

pub(crate) fn file_kind(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
        | "avif" => "image",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => "audio",
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "m4v" | "mpg" | "mpeg" | "wmv" | "flv" => "video",
        _ => "file",
    }
}

pub(crate) fn stable_hash(bytes: &[u8]) -> u64 {
    hash_bytes(0xcbf29ce484222325, bytes.iter().copied())
}

fn hash_bytes(initial: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(initial, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn text_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
}

fn text_simhash(text: &str) -> u64 {
    if text_tokens(text).take(8).count() < 8 {
        return byte_simhash(text.as_bytes());
    }
    let mut weights = [0i64; 64];
    let mut previous = None;
    for token in text_tokens(text) {
        let hash = hash_bytes(
            0xcbf29ce484222325,
            token.bytes().map(|byte| byte.to_ascii_lowercase()),
        );
        apply_feature(&mut weights, hash, 4);
        if let Some(previous_hash) = previous {
            let prefix = hash_bytes(previous_hash, std::iter::once(0));
            apply_feature(
                &mut weights,
                hash_bytes(prefix, token.bytes().map(|byte| byte.to_ascii_lowercase())),
                1,
            );
        }
        previous = Some(hash);
    }
    collapse_weights(weights)
}

fn byte_simhash(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut weights = [0i64; 64];
    for chunk in bytes.chunks(8) {
        apply_feature(&mut weights, stable_hash(chunk), chunk.len() as i64);
    }
    collapse_weights(weights)
}

fn media_simhash(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut weights = [0i64; 64];
    for byte in bytes.iter().step_by((bytes.len() / 8_192).max(1)) {
        apply_feature(&mut weights, stable_hash(&[b'h', byte / 16]), 2);
    }
    let window_size = bytes.len().clamp(16, 128);
    let sample_count = 32.min(bytes.len());
    for sample in 0..sample_count {
        let start = if sample_count <= 1 {
            0
        } else {
            sample * bytes.len().saturating_sub(window_size) / (sample_count - 1)
        };
        apply_feature(
            &mut weights,
            stable_hash(&bytes[start..(start + window_size).min(bytes.len())]),
            1,
        );
    }
    collapse_weights(weights)
}

fn apply_feature(weights: &mut [i64; 64], bits: u64, weight: i64) {
    for (bit, slot) in weights.iter_mut().enumerate() {
        *slot += if (bits >> bit) & 1 == 1 {
            weight
        } else {
            -weight
        };
    }
}

fn collapse_weights(weights: [i64; 64]) -> u64 {
    weights
        .iter()
        .enumerate()
        .filter(|(_, weight)| **weight >= 0)
        .fold(0, |bits, (bit, _)| bits | (1u64 << bit))
}

fn image_dhash(bytes: &[u8]) -> Option<u64> {
    let image = image::load_from_memory(bytes)
        .ok()?
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash = 0;
    for y in 0..8 {
        for x in 0..8 {
            if image.get_pixel(x, y)[0] > image.get_pixel(x + 1, y)[0] {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }
    Some(hash)
}
