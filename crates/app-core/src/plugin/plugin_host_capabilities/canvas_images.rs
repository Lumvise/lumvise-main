//! `frontend.canvas` `add_image`: resolves an image source (URL, project
//! file, or SVG markup) without image bytes crossing the plugin boundary,
//! sizes the element from the bytes, and inserts a native Excalidraw image
//! element plus its file entry through `apply_canvas_diff`.

use std::io::Read;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use lumvise_frontend_core::FrontendCore;
use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{FRONTEND_CANVAS, exact_fields, failed, invalid, quota, required, required_string};
use crate::app::validate_media_type;

/// Untrusted external source: hard cap on fetched image bytes.
const MAX_URL_IMAGE_BYTES: usize = 25 * 1024 * 1024;
const URL_FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_URL_REDIRECTS: usize = 5;
/// Longest displayed side when neither width nor height is requested.
const MAX_FIT_SIDE: f64 = 800.0;
const FALLBACK_NATURAL_SIZE: (f64, f64) = (400.0, 300.0);

pub(super) fn add_image(
    plugin_id: &str,
    fields: &Map<String, Value>,
    frontend: &Arc<Mutex<FrontendCore>>,
) -> Result<Value, HostCapabilityError> {
    exact_fields(
        FRONTEND_CANVAS,
        fields,
        &[
            "operation",
            "canvas_id",
            "source",
            "x",
            "y",
            "width",
            "height",
            "base_revision",
        ],
    )?;
    let canvas_id = required_string(FRONTEND_CANVAS, fields, "canvas_id")?;
    let x = coordinate(fields, "x")?;
    let y = coordinate(fields, "y")?;
    let requested_width = optional_size(fields, "width")?;
    let requested_height = optional_size(fields, "height")?;
    let base_revision = optional_revision(fields)?;
    let source = required(fields, "source")?.as_object().ok_or_else(|| {
        invalid(
            FRONTEND_CANVAS,
            required(fields, "source").unwrap_or(&Value::Null),
            "an image source object",
        )
    })?;
    let (declared, bytes) = load_source(source)?;
    let media_type = validate_media_type(&declared, &bytes)
        .map_err(|message| invalid(FRONTEND_CANVAS, &json!(declared), &message))?;

    let (natural_width, natural_height) =
        image_natural_size(&media_type, &bytes).unwrap_or(FALLBACK_NATURAL_SIZE);
    let (width, height) = fit_size(
        natural_width,
        natural_height,
        requested_width,
        requested_height,
    );

    let file_id = image_file_id(&bytes);
    let element_id = format!("image-{}", random_hex(6)?);
    let now = epoch_millis();
    let data_url = format!(
        "data:{media_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let file_entry = json!({
        "id": file_id,
        "mimeType": media_type,
        "dataURL": data_url,
        "created": now,
    });
    let element = json!({
        "id": element_id,
        "type": "image",
        "x": x,
        "y": y,
        "width": width,
        "height": height,
        "angle": 0,
        "strokeColor": "transparent",
        "backgroundColor": "transparent",
        "fillStyle": "solid",
        "strokeWidth": 1,
        "strokeStyle": "solid",
        "roughness": 0,
        "opacity": 100,
        "groupIds": [],
        "frameId": null,
        "roundness": null,
        "seed": random_u32()?,
        "version": 1,
        "versionNonce": random_u32()?,
        "isDeleted": false,
        "boundElements": null,
        "updated": now,
        "link": null,
        "locked": false,
        "status": "saved",
        "fileId": file_id,
        "scale": [1, 1],
        "crop": null,
    });

    let mut frontend = frontend
        .lock()
        .map_err(|_| failed(FRONTEND_CANVAS, "frontend mutex poisoned"))?;
    let order_length = frontend
        .canvas(canvas_id)
        .document
        .get("elementOrder")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let patch = json!([
        {
            "op": "add",
            "path": format!("/files/{file_id}"),
            "value": file_entry,
        },
        {
            "op": "add",
            "path": format!("/elementsById/{element_id}"),
            "value": element,
        },
        {
            "op": "add",
            "path": format!("/elementOrder/{order_length}"),
            "value": element_id,
        },
    ]);
    let snapshot = frontend
        .apply_canvas_diff(
            canvas_id,
            patch,
            &format!("plugin:{plugin_id}"),
            base_revision,
        )
        .map_err(|error| failed(FRONTEND_CANVAS, &error.to_string()))?;
    Ok(json!({
        "element_id": element_id,
        "file_id": file_id,
        "mime_type": media_type,
        "width": width,
        "height": height,
        "revision": snapshot.revision,
    }))
}

/// Loads the requested image bytes from exactly one of `{"url"}`, `{"svg"}`,
/// or `{"project_root", "path"}`; any extra source key is rejected.
fn load_source(source: &Map<String, Value>) -> Result<(String, Vec<u8>), HostCapabilityError> {
    if let Some(url) = source.get("url") {
        exact_fields(FRONTEND_CANVAS, source, &["url"])?;
        let url = url
            .as_str()
            .ok_or_else(|| invalid(FRONTEND_CANVAS, url, "a string image URL"))?;
        return load_url(url);
    }
    if let Some(svg) = source.get("svg") {
        exact_fields(FRONTEND_CANVAS, source, &["svg"])?;
        let svg = svg
            .as_str()
            .ok_or_else(|| invalid(FRONTEND_CANVAS, svg, "a string of SVG markup"))?;
        return Ok(("image/svg+xml".to_string(), svg.as_bytes().to_vec()));
    }
    exact_fields(FRONTEND_CANVAS, source, &["project_root", "path"])?;
    let root = required_string(FRONTEND_CANVAS, source, "project_root")?;
    let path = required_string(FRONTEND_CANVAS, source, "path")?;
    load_project_file(root, path)
}

/// Fetches `url` with a bounded blocking client: http/https only, at most
/// [`MAX_URL_REDIRECTS`] redirects, 2xx only, `image/*` content only, and at
/// most [`MAX_URL_IMAGE_BYTES`] of body.
fn load_url(url: &str) -> Result<(String, Vec<u8>), HostCapabilityError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| {
        invalid(
            FRONTEND_CANVAS,
            &json!(url),
            &format!("parseable image URL: {error}"),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(invalid(
            FRONTEND_CANVAS,
            &json!(url),
            "an http or https image URL",
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(URL_FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_URL_REDIRECTS))
        .build()
        .map_err(|error| failed(FRONTEND_CANVAS, &format!("image fetch client: {error}")))?;
    let response = client
        .get(parsed)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| failed(FRONTEND_CANVAS, &format!("image fetch failed: {error}")))?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .ok_or_else(|| {
            invalid(
                FRONTEND_CANVAS,
                &json!(""),
                "an image/* response Content-Type",
            )
        })?;
    if !content_type.starts_with("image/") {
        return Err(invalid(
            FRONTEND_CANVAS,
            &json!(content_type),
            "an image/* response Content-Type",
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_URL_IMAGE_BYTES as u64)
    {
        return Err(quota(
            FRONTEND_CANVAS,
            MAX_URL_IMAGE_BYTES + 1,
            MAX_URL_IMAGE_BYTES,
        ));
    }
    let mut limited = response.take(MAX_URL_IMAGE_BYTES as u64 + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| failed(FRONTEND_CANVAS, &format!("read image body: {error}")))?;
    if bytes.len() > MAX_URL_IMAGE_BYTES {
        return Err(quota(FRONTEND_CANVAS, bytes.len(), MAX_URL_IMAGE_BYTES));
    }
    Ok((content_type, bytes))
}

/// Reads a project file relative to `root`. The path must stay inside the
/// canonical root after resolving symlinks, and the extension selects the
/// declared media type.
fn load_project_file(root: &str, path: &str) -> Result<(String, Vec<u8>), HostCapabilityError> {
    let relative = Path::new(path);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err(invalid(
            FRONTEND_CANVAS,
            &json!(path),
            "a relative project file path",
        ));
    }
    if relative
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(invalid(
            FRONTEND_CANVAS,
            &json!(path),
            "a relative project file path without .. components",
        ));
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|error| {
        failed(
            FRONTEND_CANVAS,
            &format!("canonicalize project root {root:?}: {error}"),
        )
    })?;
    let canonical_file = std::fs::canonicalize(canonical_root.join(relative)).map_err(|error| {
        invalid(
            FRONTEND_CANVAS,
            &json!(path),
            &format!("readable project file inside the project root: {error}"),
        )
    })?;
    if !canonical_file.starts_with(&canonical_root) {
        return Err(invalid(
            FRONTEND_CANVAS,
            &json!(path),
            "a project file inside the canonical project root",
        ));
    }
    let bytes = std::fs::read(&canonical_file)
        .map_err(|error| failed(FRONTEND_CANVAS, &format!("read project file: {error}")))?;
    let media_type = mime_from_extension(&canonical_file).ok_or_else(|| {
        invalid(
            FRONTEND_CANVAS,
            &json!(path),
            "a project file with a supported image extension (png, jpg, jpeg, gif, webp, avif, svg)",
        )
    })?;
    Ok((media_type.to_string(), bytes))
}

fn mime_from_extension(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Natural image size in pixels from the format header; `None` when the
/// dimensions cannot be read, so the caller falls back to 400x300.
fn image_natural_size(media_type: &str, bytes: &[u8]) -> Option<(f64, f64)> {
    let (width, height) = match media_type {
        "image/png" => png_natural_size(bytes)?,
        "image/gif" => gif_natural_size(bytes)?,
        "image/jpeg" => jpeg_natural_size(bytes)?,
        "image/webp" => webp_natural_size(bytes)?,
        "image/svg+xml" => svg_natural_size(bytes)?,
        _ => return None,
    };
    (width > 0.0 && height > 0.0).then_some((width, height))
}

/// Width and height are the big-endian `IHDR` values at the fixed offsets
/// after the PNG signature and chunk header.
fn png_natural_size(bytes: &[u8]) -> Option<(f64, f64)> {
    if bytes.len() < 24 {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((f64::from(width), f64::from(height)))
}

/// The logical screen descriptor holds little-endian width/height at
/// offsets 6 and 8.
fn gif_natural_size(bytes: &[u8]) -> Option<(f64, f64)> {
    if bytes.len() < 10 {
        return None;
    }
    let width = u16::from_le_bytes(bytes[6..8].try_into().ok()?);
    let height = u16::from_le_bytes(bytes[8..10].try_into().ok()?);
    Some((f64::from(width), f64::from(height)))
}

/// Scans the marker chain for the first start-of-frame segment, which holds
/// the big-endian height then width.
fn jpeg_natural_size(bytes: &[u8]) -> Option<(f64, f64)> {
    let mut index = 2;
    while index + 4 <= bytes.len() {
        if bytes[index] != 0xFF {
            return None;
        }
        let marker = bytes[index + 1];
        if marker == 0xFF {
            index += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0xD9 || marker == 0xDA {
            return None;
        }
        let segment_length = usize::from(u16::from_be_bytes(
            bytes[index + 2..index + 4].try_into().ok()?,
        ));
        if (0xC0..=0xCF).contains(&marker)
            && !matches!(marker, 0xC4 | 0xC8 | 0xCC)
            && index + 9 <= bytes.len()
        {
            let height = u16::from_be_bytes(bytes[index + 5..index + 7].try_into().ok()?);
            let width = u16::from_be_bytes(bytes[index + 7..index + 9].try_into().ok()?);
            return Some((f64::from(width), f64::from(height)));
        }
        index += 2 + segment_length;
    }
    None
}

/// Walks the RIFF chunks and reads the dimensions from whichever lossy
/// (`VP8 `), lossless (`VP8L`), or extended (`VP8X`) chunk appears first.
fn webp_natural_size(bytes: &[u8]) -> Option<(f64, f64)> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let chunk = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        let payload = bytes.get(offset + 8..offset + 8 + size)?;
        if chunk == b"VP8 " {
            if payload.len() >= 10 && payload[3..6] == [0x9D, 0x01, 0x2A] {
                let width = u16::from_le_bytes(payload[6..8].try_into().ok()?) & 0x3FFF;
                let height = u16::from_le_bytes(payload[8..10].try_into().ok()?) & 0x3FFF;
                return Some((f64::from(width), f64::from(height)));
            }
            return None;
        }
        if chunk == b"VP8L" {
            if payload.len() >= 5 && payload[0] == 0x2F {
                let bits = u32::from_le_bytes(payload[1..5].try_into().ok()?);
                let width = (bits & 0x3FFF) + 1;
                let height = ((bits >> 14) & 0x3FFF) + 1;
                return Some((f64::from(width), f64::from(height)));
            }
            return None;
        }
        if chunk == b"VP8X" {
            if payload.len() >= 10 {
                let width = u32::from(payload[4])
                    | (u32::from(payload[5]) << 8)
                    | (u32::from(payload[6]) << 16);
                let height = u32::from(payload[7])
                    | (u32::from(payload[8]) << 8)
                    | (u32::from(payload[9]) << 16);
                return Some((f64::from(width + 1), f64::from(height + 1)));
            }
            return None;
        }
        offset += 8 + size + (size & 1);
    }
    None
}

/// The root `<svg>` tag's `width`/`height` attributes, else the `viewBox`
/// width and height. Percent lengths have no absolute size and are skipped.
fn svg_natural_size(bytes: &[u8]) -> Option<(f64, f64)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let start = text.find("<svg")?;
    let tag = &text[start..start + text[start..].find('>')?];
    let width = svg_attribute(tag, "width").and_then(svg_length);
    let height = svg_attribute(tag, "height").and_then(svg_length);
    if let (Some(width), Some(height)) = (width, height) {
        return Some((width, height));
    }
    let view_box = svg_attribute(tag, "viewBox")?;
    let mut values = view_box.split_whitespace();
    let _min_x: f64 = values.next()?.parse().ok()?;
    let _min_y: f64 = values.next()?.parse().ok()?;
    Some((values.next()?.parse().ok()?, values.next()?.parse().ok()?))
}

/// Reads `name="value"` (or single quotes) from a tag fragment. The name must
/// start at a whitespace boundary so `stroke-width` is not read as `width`.
fn svg_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut search = tag;
    while let Some(index) = search.find(name) {
        let at_boundary = index == 0
            || search[..index]
                .chars()
                .next_back()
                .is_some_and(|char| char.is_whitespace());
        if at_boundary
            && let Some(value) = search[index + name.len()..].trim_start().strip_prefix('=')
            && let Some(quote) = value.trim_start().chars().next()
            && (quote == '"' || quote == '\'')
            && let Some(value) = value.trim_start().get(1..)
            && let Some(end) = value.find(quote)
        {
            return Some(&value[..end]);
        }
        search = &search[index + name.len()..];
    }
    None
}

fn svg_length(value: &str) -> Option<f64> {
    let value = value.trim();
    let end = value
        .find(|char: char| !(char.is_ascii_digit() || matches!(char, '.' | '-' | '+')))
        .unwrap_or(value.len());
    if &value[end..] == "%" {
        return None;
    }
    value[..end]
        .parse::<f64>()
        .ok()
        .filter(|length| *length > 0.0)
}

/// Requested sizes win; one missing side keeps the natural aspect ratio;
/// with neither, the natural size scales down to fit [`MAX_FIT_SIDE`] on the
/// longer side.
fn fit_size(
    natural_width: f64,
    natural_height: f64,
    requested_width: Option<f64>,
    requested_height: Option<f64>,
) -> (f64, f64) {
    match (requested_width, requested_height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, width * natural_height / natural_width),
        (None, Some(height)) => (height * natural_width / natural_height, height),
        (None, None) => {
            let longest = natural_width.max(natural_height);
            if longest <= MAX_FIT_SIDE {
                (natural_width, natural_height)
            } else {
                let scale = MAX_FIT_SIDE / longest;
                (natural_width * scale, natural_height * scale)
            }
        }
    }
}

/// Canvas file ids are the first 40 hex characters of the SHA-256.
fn image_file_id(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))[..40].to_string()
}

fn coordinate(fields: &Map<String, Value>, key: &str) -> Result<f64, HostCapabilityError> {
    let value = required(fields, key)?;
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| invalid(FRONTEND_CANVAS, value, &format!("finite number {key}")))
}

fn optional_size(
    fields: &Map<String, Value>,
    key: &str,
) -> Result<Option<f64>, HostCapabilityError> {
    match fields.get(key).filter(|value| !value.is_null()) {
        Some(value) => Ok(Some(
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .ok_or_else(|| {
                    invalid(
                        FRONTEND_CANVAS,
                        value,
                        &format!("non-negative finite number {key}"),
                    )
                })?,
        )),
        None => Ok(None),
    }
}

fn optional_revision(fields: &Map<String, Value>) -> Result<Option<u64>, HostCapabilityError> {
    fields
        .get("base_revision")
        .filter(|value| !value.is_null())
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                invalid(FRONTEND_CANVAS, value, "non-negative integer base_revision")
            })
        })
        .transpose()
}

fn random_hex(count: usize) -> Result<String, HostCapabilityError> {
    let mut random = vec![0_u8; count];
    getrandom::fill(&mut random).map_err(|error| {
        failed(
            FRONTEND_CANVAS,
            &format!("canvas image identifier generator: {error}"),
        )
    })?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn random_u32() -> Result<u32, HostCapabilityError> {
    let mut random = [0_u8; 4];
    getrandom::fill(&mut random).map_err(|error| {
        failed(
            FRONTEND_CANVAS,
            &format!("canvas image nonce generator: {error}"),
        )
    })?;
    Ok(u32::from_le_bytes(random))
}

fn epoch_millis() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginHostServices;
    use lumvise_db_core::LocalPersistence;
    use lumvise_neural_core::LlmProviderRegistry;
    use serde_json::json;
    use std::io::Write;
    use std::sync::Arc;

    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// Minimal 24-byte PNG header: signature + IHDR chunk with the given
    /// big-endian dimensions.
    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        [
            PNG_SIGNATURE,
            &0x00_00_00_0D_u32.to_be_bytes(),
            b"IHDR",
            &width.to_be_bytes(),
            &height.to_be_bytes(),
        ]
        .concat()
    }

    fn canvas_services() -> (
        Arc<PluginHostServices>,
        Arc<dyn lumvise_db_core::SemanticPersistence>,
    ) {
        let persistence = Arc::new(LocalPersistence::in_memory().expect("test persistence"));
        let semantic: Arc<dyn lumvise_db_core::SemanticPersistence> = persistence.clone();
        (
            PluginHostServices::new(
                FrontendCore::default(),
                LlmProviderRegistry::empty(),
                Arc::clone(&semantic),
            ),
            semantic,
        )
    }

    fn invoke_canvas(services: &PluginHostServices, input: Value) -> Result<Value, String> {
        services
            .invoke("plugin.test", FRONTEND_CANVAS, input)
            .map_err(|error| error.to_string())
    }

    fn add_image_input(source: Value, extra: Value) -> Value {
        let mut input = json!({
            "operation": "add_image",
            "canvas_id": "main",
            "source": source,
            "x": 10.0,
            "y": 20.0,
        });
        let extra = extra.as_object().expect("extra object").clone();
        for (key, value) in extra {
            input[key] = value;
        }
        input
    }

    fn expected_file_id(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))[..40].to_string()
    }

    #[test]
    fn add_image_from_svg_inserts_element_and_file() {
        let (services, _semantic) = canvas_services();
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"200\" height=\"100\"></svg>";

        let result = invoke_canvas(
            &services,
            add_image_input(json!({"svg": svg}), json!({"width": null, "height": null})),
        )
        .expect("svg image added");

        assert_eq!(result["mime_type"], "image/svg+xml");
        assert_eq!(result["width"], 200.0);
        assert_eq!(result["height"], 100.0);
        assert_eq!(result["revision"], 1);
        assert_eq!(result["element_id"].as_str().unwrap().len(), 18);
        assert!(result["element_id"].as_str().unwrap().starts_with("image-"));
        let file_id = result["file_id"].as_str().unwrap();
        assert_eq!(file_id.len(), 40);
        assert_eq!(file_id, expected_file_id(svg.as_bytes()));

        let canvas = invoke_canvas(&services, json!({"operation": "get"})).unwrap();
        let file = &canvas["document"]["files"][file_id];
        assert_eq!(file["mimeType"], "image/svg+xml");
        assert!(file["created"].as_u64().is_some());
        assert!(
            file["dataURL"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,")
        );
        let element = &canvas["document"]["elementsById"][result["element_id"].as_str().unwrap()];
        assert_eq!(element["type"], "image");
        assert_eq!(element["fileId"], file_id);
        assert_eq!(element["status"], "saved");
        assert_eq!(element["scale"], json!([1, 1]));
        assert_eq!(element["x"], 10.0);
        assert_eq!(element["y"], 20.0);
        assert_eq!(canvas["document"]["elementOrder"][0], result["element_id"]);
    }

    #[test]
    fn add_image_keeps_aspect_ratio_when_one_side_is_requested() {
        let (services, _semantic) = canvas_services();
        let svg = "<svg width=\"200\" height=\"100\"></svg>";

        let only_width = invoke_canvas(
            &services,
            add_image_input(json!({"svg": svg}), json!({"width": 50.0})),
        )
        .unwrap();
        assert_eq!(only_width["width"], 50.0);
        assert_eq!(only_width["height"], 25.0);

        let only_height = invoke_canvas(
            &services,
            add_image_input(json!({"svg": svg}), json!({"height": 50.0})),
        )
        .unwrap();
        assert_eq!(only_height["width"], 100.0);
        assert_eq!(only_height["height"], 50.0);
    }

    #[test]
    fn add_image_scales_large_natural_size_to_fit() {
        let (services, _semantic) = canvas_services();
        let svg = "<svg viewBox=\"0 0 1600 800\"></svg>";

        let result =
            invoke_canvas(&services, add_image_input(json!({"svg": svg}), json!({}))).unwrap();
        assert_eq!(result["width"], 800.0);
        assert_eq!(result["height"], 400.0);
    }

    #[test]
    fn add_image_from_project_png_reads_natural_size() {
        let (services, _semantic) = canvas_services();
        let root = tempfile::tempdir().expect("temp root");
        std::fs::write(root.path().join("image.png"), png_bytes(8, 4)).unwrap();

        let result = invoke_canvas(
            &services,
            add_image_input(
                json!({"project_root": root.path().to_str().unwrap(), "path": "image.png"}),
                json!({}),
            ),
        )
        .expect("project png added");

        assert_eq!(result["mime_type"], "image/png");
        assert_eq!(result["width"], 8.0);
        assert_eq!(result["height"], 4.0);
    }

    #[test]
    fn add_image_rejects_traversal_and_absolute_and_outside_paths() {
        let (services, _semantic) = canvas_services();
        let root = tempfile::tempdir().expect("temp root");
        std::fs::write(root.path().join("image.png"), png_bytes(8, 4)).unwrap();
        let outside = tempfile::tempdir().expect("outside root");
        let outside_png = outside.path().join("outside.png");
        std::fs::write(&outside_png, png_bytes(2, 1)).unwrap();

        for path in [
            "../escape.png",
            "/etc/passwd",
            outside_png.to_str().unwrap(),
        ] {
            let error = invoke_canvas(
                &services,
                add_image_input(
                    json!({"project_root": root.path().to_str().unwrap(), "path": path}),
                    json!({}),
                ),
            )
            .expect_err("path must be rejected");
            assert!(
                error.contains("relative") || error.contains("inside"),
                "{path}: {error}"
            );
        }

        // A symlink inside the root pointing outside must also be rejected.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_png, root.path().join("link.png")).unwrap();
            let error = invoke_canvas(
                &services,
                add_image_input(
                    json!({"project_root": root.path().to_str().unwrap(), "path": "link.png"}),
                    json!({}),
                ),
            )
            .expect_err("symlink escape must be rejected");
            assert!(error.contains("inside"));
        }
    }

    #[test]
    fn add_image_rejects_unknown_extensions_and_missing_files() {
        let (services, _semantic) = canvas_services();
        let root = tempfile::tempdir().expect("temp root");
        std::fs::write(root.path().join("notes.txt"), b"not an image").unwrap();

        let error = invoke_canvas(
            &services,
            add_image_input(
                json!({"project_root": root.path().to_str().unwrap(), "path": "notes.txt"}),
                json!({}),
            ),
        )
        .expect_err("unsupported extension must be rejected");
        assert!(error.contains("extension") || error.contains("media type"));

        let error = invoke_canvas(
            &services,
            add_image_input(
                json!({"project_root": root.path().to_str().unwrap(), "path": "missing.png"}),
                json!({}),
            ),
        )
        .expect_err("missing file must be rejected");
        assert!(error.contains("readable"));
    }

    #[test]
    fn add_image_rejects_base_revision_mismatch_like_apply_diff() {
        let (services, _semantic) = canvas_services();
        let error = invoke_canvas(
            &services,
            add_image_input(
                json!({"svg": "<svg width=\"10\" height=\"10\"></svg>"}),
                json!({"base_revision": 5}),
            ),
        )
        .expect_err("stale base revision must be rejected");
        assert!(error.contains("revision"), "{error}");
    }

    #[test]
    fn add_image_rejects_invalid_svg_bytes_and_bad_request_sizes() {
        let (services, _semantic) = canvas_services();

        let error = invoke_canvas(
            &services,
            add_image_input(json!({"svg": "not svg at all"}), json!({})),
        )
        .expect_err("non-SVG bytes must be rejected");
        assert!(error.contains("media type"), "{error}");

        let error = invoke_canvas(
            &services,
            add_image_input(
                json!({"svg": "<svg width=\"10\" height=\"10\"></svg>"}),
                json!({"width": -1.0}),
            ),
        )
        .expect_err("negative width must be rejected");
        assert!(error.contains("non-negative"), "{error}");
    }

    #[test]
    fn add_image_rejects_extra_source_keys() {
        let (services, _semantic) = canvas_services();
        let error = invoke_canvas(
            &services,
            add_image_input(
                json!({"svg": "<svg width=\"10\" height=\"10\"></svg>", "url": "http://x"}),
                json!({}),
            ),
        )
        .expect_err("mixed sources must be rejected");
        assert!(error.contains("only fields"), "{error}");
    }

    #[test]
    fn add_image_rejects_non_http_schemes() {
        let (services, _semantic) = canvas_services();
        for url in ["ftp://example.com/a.png", "file:///tmp/a.png"] {
            let error = invoke_canvas(&services, add_image_input(json!({"url": url}), json!({})))
                .expect_err("non-http scheme must be rejected");
            assert!(error.contains("http"), "{url}: {error}");
        }
    }

    /// One-shot stub HTTP server: accepts one connection, answers it with a
    /// canned response, and shuts down.
    fn stub_server(response: &'static [u8]) -> String {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind stub");
        let address = listener.local_addr().expect("stub address");
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("stub connection");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = std::io::Read::read(&mut socket, &mut buffer).expect("stub read");
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            socket.write_all(response).expect("stub write");
        });
        format!("http://{address}/image.png")
    }

    const STUB_PNG_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 24\r\n\r\n\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x02\x00\x00\x00\x01";

    const STUB_HTML_RESPONSE: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 2\r\n\r\nhi";

    #[test]
    fn add_image_from_url_stores_image_bytes() {
        let (services, _semantic) = canvas_services();
        let url = stub_server(STUB_PNG_RESPONSE);

        let result = invoke_canvas(&services, add_image_input(json!({"url": url}), json!({})))
            .expect("url image added");

        assert_eq!(result["mime_type"], "image/png");
        assert_eq!(result["width"], 2.0);
        assert_eq!(result["height"], 1.0);
    }

    #[test]
    fn add_image_rejects_non_image_content_types_from_urls() {
        let (services, _semantic) = canvas_services();
        let url = stub_server(STUB_HTML_RESPONSE);

        let error = invoke_canvas(&services, add_image_input(json!({"url": url}), json!({})))
            .expect_err("non-image content type must be rejected");
        assert!(error.contains("image/*"), "{error}");
    }

    #[test]
    fn export_files_on_an_empty_canvas_returns_an_empty_map() {
        let (services, _semantic) = canvas_services();

        let exported = invoke_canvas(
            &services,
            json!({"operation": "export_files", "artifact_id": "canvas-1"}),
        )
        .expect("empty export succeeds");

        assert_eq!(exported["files"], json!({}));
    }

    #[test]
    fn export_files_stores_each_file_and_refs_read_back_to_the_same_bytes() {
        let (services, semantic) = canvas_services();
        let png = png_bytes(4, 2);
        let png_data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        let svg_data_url =
            "data:image/svg+xml,%3Csvg%20xmlns=%22http://www.w3.org/2000/svg%22%3E%3C/svg%3E";
        invoke_canvas(
            &services,
            json!({
                "operation": "apply_diff",
                "canvas_id": "main",
                "patch": [
                    {"op": "add", "path": "/files/png-entry", "value": {"id": "png-entry", "mimeType": "image/png", "dataURL": png_data_url}},
                    {"op": "add", "path": "/files/svg-entry", "value": {"id": "svg-entry", "mimeType": "image/svg+xml", "dataURL": svg_data_url}}
                ]
            }),
        )
        .expect("file entries placed");

        let exported = invoke_canvas(
            &services,
            json!({"operation": "export_files", "artifact_id": "canvas-1"}),
        )
        .expect("export succeeds");

        let png_entry = &exported["files"]["png-entry"];
        assert_eq!(png_entry["mimeType"], "image/png");
        let png_ref = png_entry["contentRef"].as_str().unwrap();
        assert!(png_ref.starts_with("canvas-file:canvas-1:"));
        let svg_entry = &exported["files"]["svg-entry"];
        assert_eq!(svg_entry["mimeType"], "image/svg+xml");
        let svg_ref = svg_entry["contentRef"].as_str().unwrap();

        let (media_type, stored) = crate::app::read_canvas_file_with(semantic.as_ref(), png_ref)
            .unwrap()
            .expect("png blob stored");
        assert_eq!(media_type, "image/png");
        assert_eq!(stored, png);
        let (media_type, stored) = crate::app::read_canvas_file_with(semantic.as_ref(), svg_ref)
            .unwrap()
            .expect("svg blob stored");
        assert_eq!(media_type, "image/svg+xml");
        assert_eq!(stored, svg.as_bytes());
    }

    #[test]
    fn export_files_rejects_an_undecodable_entry_naming_the_file() {
        let (services, _semantic) = canvas_services();
        invoke_canvas(
            &services,
            json!({
                "operation": "apply_diff",
                "canvas_id": "main",
                "patch": [
                    {"op": "add", "path": "/files/broken-entry", "value": {"id": "broken-entry", "mimeType": "image/png", "dataURL": "data:image/png"}}
                ]
            }),
        )
        .expect("file entry placed");

        let error = invoke_canvas(
            &services,
            json!({"operation": "export_files", "artifact_id": "canvas-1"}),
        )
        .expect_err("undecodable entry must be rejected");

        assert!(error.contains("broken-entry"), "{error}");
    }

    #[test]
    fn export_files_rejects_bytes_that_do_not_match_the_declared_type() {
        let (services, _semantic) = canvas_services();
        let jpeg = [0xFF_u8, 0xD8, 0xFF];
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(jpeg)
        );
        invoke_canvas(
            &services,
            json!({
                "operation": "apply_diff",
                "canvas_id": "main",
                "patch": [
                    {"op": "add", "path": "/files/mislabeled", "value": {"id": "mislabeled", "mimeType": "image/png", "dataURL": data_url}}
                ]
            }),
        )
        .expect("file entry placed");

        let error = invoke_canvas(
            &services,
            json!({"operation": "export_files", "artifact_id": "canvas-1"}),
        )
        .expect_err("mislabeled entry must be rejected");

        assert!(error.contains("mislabeled"), "{error}");
        assert!(error.contains("do not match"), "{error}");
    }
}
