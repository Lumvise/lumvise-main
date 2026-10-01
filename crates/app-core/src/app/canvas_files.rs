//! Canvas image-file storage for workspace canvases.
//!
//! Pasted or generated canvas images persist as one db-core artifact blob per
//! image. The content reference embeds the owning canvas artifact id and the
//! SHA-256 of the bytes, so writing the same image to the same canvas again is
//! idempotent: identical bytes always produce the same
//! `canvas-file:<artifact_id>:<sha256>` ref. The bytes are validated against
//! the declared image media type with magic-byte checks so the blob store never
//! holds mislabeled content.
//!
//! `db-core` remains the only persistence seam: writes and reads go through
//! the existing [`crate::endpoints::database::DatabaseEndpoints`] `put_blob` /
//! `blob` wrapper (`app.database()`), never raw SQL or plugin storage.
//!
//! The plugin-host path has no [`AppCore`]; [`put_canvas_file_with`] /
//! [`read_canvas_file_with`] run over the semantic persistence the broker's
//! services already hold, and [`export_canvas_files`] persists the current
//! conversation canvas's `files` map through the same seam.

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::{AppCore, AppCoreError, Result};
use base64::Engine;
use lumvise_db_core::{SemanticOperation, SemanticPersistence, SemanticResult};
use lumvise_resource_routing::InvocationControl;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const CANVAS_FILE_ENDPOINT: &str = "/api/canvas-file";
const CANVAS_FILE_REF_PREFIX: &str = "canvas-file:";

/// Describes one stored canvas image file.
///
/// Serializes camelCase for the renderer: `{"contentRef","mimeType","byteSize"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CanvasFileRef {
    pub(crate) content_ref: String,
    pub(crate) mime_type: String,
    pub(crate) byte_size: usize,
}

/// Stores `bytes` as the canvas `<artifact_id>`'s image file and returns its
/// stable content reference. The same bytes for the same canvas produce the
/// same ref; empty ids/bytes and mislabeled media types are rejected.
pub(crate) fn put_canvas_file(
    app: &AppCore,
    artifact_id: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<CanvasFileRef> {
    put_canvas_file_with(app.semantic.as_ref(), artifact_id, media_type, bytes)
}

/// [`put_canvas_file`] over any semantic persistence, so host-owned paths
/// without an [`AppCore`] (the plugin broker's services) share one storage
/// implementation.
pub(crate) fn put_canvas_file_with(
    semantic: &dyn SemanticPersistence,
    artifact_id: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<CanvasFileRef> {
    let artifact_id = artifact_id.trim();
    if artifact_id.is_empty() {
        return Err(AppCoreError::invalid_value(
            "canvas artifact id",
            "a non-empty canvas artifact id",
        ));
    }
    if bytes.is_empty() {
        return Err(AppCoreError::invalid_value(
            "canvas file bytes",
            "non-empty image bytes",
        ));
    }
    let media_type = validate_media_type(media_type, bytes)
        .map_err(|message| AppCoreError::invalid_value("declared media type", message))?;
    let digest = hex::encode(Sha256::digest(bytes));
    let content_ref = format!("{CANVAS_FILE_REF_PREFIX}{artifact_id}:{digest}");
    match semantic.execute(
        SemanticOperation::ArtifactBlobPut {
            content_ref: content_ref.clone(),
            artifact_id: artifact_id.to_string(),
            media_type: media_type.clone(),
            content: bytes.to_vec(),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        SemanticResult::ArtifactBlob(Some(_)) => Ok(CanvasFileRef {
            content_ref,
            mime_type: media_type,
            byte_size: bytes.len(),
        }),
        SemanticResult::ArtifactBlob(None) => Err(AppCoreError::missing_value(
            content_ref,
            "blob persisted by semantic persistence",
        )),
        other => Err(AppCoreError::unsupported(
            "store canvas file",
            format!("matching semantic persistence result, got {other:?}"),
        )),
    }
}

/// Reads a canvas image file's stored media type and bytes.
///
/// Only `canvas-file:` refs are accepted, so callers cannot smuggle other blob
/// namespaces through this seam. Missing refs return `Ok(None)`.
pub(crate) fn read_canvas_file(
    app: &AppCore,
    content_ref: &str,
) -> Result<Option<(String, Vec<u8>)>> {
    read_canvas_file_with(app.semantic.as_ref(), content_ref)
}

/// [`read_canvas_file`] over any semantic persistence; see
/// [`put_canvas_file_with`].
pub(crate) fn read_canvas_file_with(
    semantic: &dyn SemanticPersistence,
    content_ref: &str,
) -> Result<Option<(String, Vec<u8>)>> {
    if !content_ref.starts_with(CANVAS_FILE_REF_PREFIX) {
        return Err(AppCoreError::invalid_value(
            "canvas file reference",
            format!("a reference starting with {CANVAS_FILE_REF_PREFIX:?}"),
        ));
    }
    match semantic.execute(
        SemanticOperation::ArtifactBlobGet {
            content_ref: content_ref.to_string(),
        },
        &InvocationControl::sixty_seconds(),
    )? {
        SemanticResult::ArtifactBlob(blob) => Ok(blob.map(|blob| (blob.media_type, blob.content))),
        other => Err(AppCoreError::unsupported(
            "read canvas file",
            format!("matching semantic persistence result, got {other:?}"),
        )),
    }
}

/// Decodes a canvas data URL into its media type and bytes. Supports the
/// `data:<mime>;base64,<b64>` form and the plain `data:image/svg+xml,`
/// percent-encoded or raw UTF-8 form.
pub(crate) fn decode_data_url(data_url: &str) -> std::result::Result<(String, Vec<u8>), String> {
    let Some(rest) = data_url.strip_prefix("data:") else {
        return Err("missing data: prefix".to_string());
    };
    let Some((header, payload)) = rest.split_once(',') else {
        return Err("missing data URL payload separator".to_string());
    };
    let mut segments = header.split(';');
    let mime = segments
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if mime.is_empty() {
        return Err("missing data URL media type".to_string());
    }
    if segments.any(|segment| segment.trim() == "base64") {
        let compact: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(compact.as_bytes())
            .map_err(|error| format!("invalid base64 canvas file payload: {error}"))?;
        Ok((mime, bytes))
    } else {
        Ok((mime, percent_decode(payload)))
    }
}

/// Decodes percent-escaped bytes, copying every other byte unchanged so raw
/// UTF-8 SVG payloads pass through intact.
fn percent_decode(payload: &str) -> Vec<u8> {
    let payload = payload.as_bytes();
    let mut decoded = Vec::with_capacity(payload.len());
    let mut index = 0;
    while index < payload.len() {
        if payload[index] == b'%'
            && index + 2 < payload.len()
            && let (Some(high), Some(low)) =
                (hex_digit(payload[index + 1]), hex_digit(payload[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(payload[index]);
            index += 1;
        }
    }
    decoded
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Stores every entry in a conversation canvas document's `files` map
/// (Excalidraw `BinaryFileData`) and returns the plugin-facing
/// `{fileId: {"mimeType", "contentRef"}}` map. One undecodable or
/// invalid-image entry rejects the whole export with the offending fileId
/// named.
pub(crate) fn export_canvas_files(
    semantic: &dyn SemanticPersistence,
    artifact_id: &str,
    document: &Value,
) -> Result<Value> {
    let mut files = serde_json::Map::new();
    if let Some(entries) = document.get("files").and_then(Value::as_object) {
        for (file_id, entry) in entries {
            let data_url = entry
                .get("dataURL")
                .and_then(Value::as_str)
                .ok_or_else(|| canvas_file_error(file_id, "missing dataURL field"))?;
            let (mime, bytes) = decode_data_url(data_url)
                .map_err(|message| canvas_file_error(file_id, &message))?;
            let reference = put_canvas_file_with(semantic, artifact_id, &mime, &bytes)
                .map_err(|error| canvas_file_error(file_id, &error.to_string()))?;
            files.insert(
                file_id.clone(),
                json!({"mimeType": reference.mime_type, "contentRef": reference.content_ref}),
            );
        }
    }
    Ok(json!({ "files": Value::Object(files) }))
}

fn canvas_file_error(file_id: &str, message: &str) -> AppCoreError {
    AppCoreError::invalid_value(file_id, format!("valid canvas file entry: {message}"))
}

/// Validates the declared media type against the bytes and returns its
/// canonical (lowercase) form, or the rejection reason.
pub(crate) fn validate_media_type(
    media_type: &str,
    bytes: &[u8],
) -> std::result::Result<String, String> {
    let canonical = media_type.trim().to_ascii_lowercase();
    let matches = match canonical.as_str() {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/avif" => {
            bytes.len() >= 12
                && &bytes[4..8] == b"ftyp"
                && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis")
        }
        "image/svg+xml" => is_svg(bytes),
        other => {
            return Err(format!(
                "unsupported canvas media type {other:?}; expected image/png, image/jpeg, image/gif, image/webp, image/avif, or image/svg+xml"
            ));
        }
    };
    if matches {
        Ok(canonical)
    } else {
        Err(format!(
            "image bytes do not match the declared media type {canonical:?}"
        ))
    }
}

/// SVG is UTF-8 text starting (after any BOM and whitespace) with `<` and
/// containing an `<svg` element.
fn is_svg(bytes: &[u8]) -> bool {
    let bytes = bytes
        .strip_prefix([0xEF, 0xBB, 0xBF].as_slice())
        .unwrap_or(bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let trimmed = text.trim_start();
    trimmed.starts_with('<') && trimmed.contains("<svg")
}

pub(crate) fn canvas_file_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        );
    }
    match request.method.as_str() {
        "GET" => get_canvas_file_response(app, request),
        "PUT" => put_canvas_file_response(app, request),
        _ => error_response(
            "405 Method Not Allowed",
            "canvas file endpoint supports GET and PUT",
        ),
    }
}

fn get_canvas_file_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let Some(content_ref) = non_empty(request, "contentRef") else {
        return error_response("400 Bad Request", "contentRef is required");
    };
    if !content_ref.starts_with(CANVAS_FILE_REF_PREFIX) {
        return error_response(
            "400 Bad Request",
            format!("{content_ref} is not a canvas-file: content reference"),
        );
    }
    match read_canvas_file(app, &content_ref) {
        Ok(Some((media_type, bytes))) => HttpResponse::buffered(
            "200 OK",
            media_type,
            bytes,
            vec![
                ("X-Content-Type-Options".into(), "nosniff".into()),
                ("Cache-Control".into(), "no-store".into()),
            ],
        ),
        Ok(None) => error_response(
            "404 Not Found",
            format!("canvas file {content_ref} was not found"),
        ),
        Err(error) => error_response("400 Bad Request", error.to_string()),
    }
}

fn put_canvas_file_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    let Some(artifact_id) = non_empty(request, "artifactId") else {
        return error_response("400 Bad Request", "artifactId is required");
    };
    let Some(media_type) = non_empty(request, "mimeType") else {
        return error_response("400 Bad Request", "mimeType is required");
    };
    if request.body.is_empty() {
        return error_response("400 Bad Request", "canvas file body is empty");
    }
    if let Err(message) = validate_media_type(&media_type, &request.body) {
        return error_response("415 Unsupported Media Type", message);
    }
    match put_canvas_file(app, &artifact_id, &media_type, &request.body) {
        Ok(reference) => match serde_json::to_value(reference) {
            Ok(value) => json_response("200 OK", value),
            Err(error) => error_response("500 Internal Server Error", error.to_string()),
        },
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    }
}

fn non_empty(request: &HttpRequest, key: &str) -> Option<String> {
    request
        .query
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppCore;
    use std::collections::BTreeMap;

    const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPEG_HEADER: &[u8] = &[0xFF, 0xD8, 0xFF];

    fn png_bytes() -> Vec<u8> {
        [PNG_HEADER, b"fake-idat"].concat()
    }

    fn app() -> AppCore {
        AppCore::in_memory().expect("in-memory app")
    }

    #[test]
    fn puts_and_reads_a_png_round_trip() {
        let app = app();
        let bytes = png_bytes();
        let reference = put_canvas_file(&app, "canvas-1", "image/png", &bytes).unwrap();
        assert_eq!(reference.mime_type, "image/png");
        assert_eq!(reference.byte_size, bytes.len());
        assert!(reference.content_ref.starts_with("canvas-file:canvas-1:"));
        let (media_type, stored) = read_canvas_file(&app, &reference.content_ref)
            .unwrap()
            .unwrap();
        assert_eq!(media_type, "image/png");
        assert_eq!(stored, bytes);
    }

    #[test]
    fn same_bytes_produce_the_same_ref() {
        let app = app();
        let bytes = png_bytes();
        let first = put_canvas_file(&app, "canvas-1", "image/png", &bytes).unwrap();
        let second = put_canvas_file(&app, "canvas-1", "image/png", &bytes).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn different_bytes_produce_different_refs() {
        let app = app();
        let first = put_canvas_file(&app, "canvas-1", "image/png", &png_bytes()).unwrap();
        let second = put_canvas_file(
            &app,
            "canvas-1",
            "image/png",
            &[PNG_HEADER, b"other"].concat(),
        )
        .unwrap();
        assert_ne!(first.content_ref, second.content_ref);
    }

    #[test]
    fn accepts_an_svg_document() {
        let app = app();
        let svg = b"\xEF\xBB\xBF\n  <svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        let reference = put_canvas_file(&app, "canvas-1", "image/svg+xml", svg).unwrap();
        assert_eq!(reference.mime_type, "image/svg+xml");
        let (media_type, stored) = read_canvas_file(&app, &reference.content_ref)
            .unwrap()
            .unwrap();
        assert_eq!(media_type, "image/svg+xml");
        assert_eq!(stored, svg.as_slice());
    }

    #[test]
    fn rejects_bytes_that_do_not_match_the_declared_type() {
        let app = app();
        let error = put_canvas_file(&app, "canvas-1", "image/png", JPEG_HEADER)
            .expect_err("jpeg bytes declared png must be rejected");
        assert!(error.to_string().contains("do not match"));
    }

    #[test]
    fn rejects_unknown_media_types() {
        let app = app();
        let error = put_canvas_file(&app, "canvas-1", "image/x-icon", &png_bytes())
            .expect_err("unknown media type must be rejected");
        assert!(error.to_string().contains("unsupported canvas media type"));
    }

    #[test]
    fn rejects_empty_bytes_and_empty_artifact_ids() {
        let app = app();
        assert!(put_canvas_file(&app, "canvas-1", "image/png", &[]).is_err());
        assert!(put_canvas_file(&app, "  ", "image/png", &png_bytes()).is_err());
    }

    #[test]
    fn reads_only_canvas_file_refs() {
        let app = app();
        let error = read_canvas_file(&app, "blob://other")
            .expect_err("non canvas-file refs must be rejected");
        assert!(error.to_string().contains("canvas-file:"));
    }

    #[test]
    fn decodes_base64_and_plain_svg_data_urls() {
        let bytes = png_bytes();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let (mime, decoded) = decode_data_url(&format!("data:image/png;base64,{encoded}")).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(decoded, bytes);

        let (mime, decoded) = decode_data_url("data:image/svg+xml,%3Csvg%3E%3C/svg%3E").unwrap();
        assert_eq!(mime, "image/svg+xml");
        assert_eq!(decoded, b"<svg></svg>");

        let (mime, decoded) = decode_data_url("data:image/svg+xml,<svg>raw</svg>").unwrap();
        assert_eq!(mime, "image/svg+xml");
        assert_eq!(decoded, b"<svg>raw</svg>");
    }

    #[test]
    fn rejects_data_urls_without_prefix_separator_or_type() {
        assert!(decode_data_url("image/png;base64,AAA=").is_err());
        assert!(decode_data_url("data:image/png").is_err());
        assert!(decode_data_url("data:;base64,AAA=").is_err());
        assert!(
            decode_data_url("data:image/png;base64,not!base64").is_err(),
            "invalid base64 payload must be rejected"
        );
    }

    #[test]
    fn reports_missing_refs_as_none() {
        let app = app();
        assert!(
            read_canvas_file(&app, "canvas-file:canvas-1:deadbeef")
                .unwrap()
                .is_none()
        );
    }

    fn request(method: &str, query: Vec<(&str, &str)>, body: &[u8]) -> HttpRequest {
        let mut params = BTreeMap::new();
        for (key, value) in query {
            params.insert(key.to_string(), value.to_string());
        }
        HttpRequest {
            method: method.to_string(),
            path: CANVAS_FILE_ENDPOINT.to_string(),
            query: params,
            authorization: Some("Bearer canvas-file-test".to_string()),
            body: body.to_vec(),
        }
    }

    fn credentialed_app() -> AppCore {
        let app = app();
        app.install_bridge_credential_store(std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )))
        .expect("credential store");
        app.set_bridge_credential(Some("canvas-file-test".into()), u64::MAX)
            .expect("credential");
        app
    }

    #[test]
    fn route_put_then_get_round_trips_the_bytes() {
        let app = credentialed_app();
        let bytes = png_bytes();
        let put = canvas_file_response(
            &app,
            &request(
                "PUT",
                vec![("artifactId", "canvas-1"), ("mimeType", "image/png")],
                &bytes,
            ),
        );
        assert_eq!(put.status, "200 OK");
        let payload: serde_json::Value =
            serde_json::from_slice(put.buffered_bytes().unwrap()).unwrap();
        assert_eq!(payload["mimeType"], "image/png");
        assert_eq!(payload["byteSize"], bytes.len());
        let content_ref = payload["contentRef"].as_str().unwrap().to_string();
        assert!(content_ref.starts_with("canvas-file:canvas-1:"));

        let get = canvas_file_response(
            &app,
            &request("GET", vec![("contentRef", content_ref.as_str())], &[]),
        );
        assert_eq!(get.status, "200 OK");
        assert_eq!(get.content_type, "image/png");
        assert_eq!(get.buffered_bytes().unwrap(), bytes);
    }

    #[test]
    fn route_reports_missing_refs_as_not_found() {
        let app = credentialed_app();
        let get = canvas_file_response(
            &app,
            &request(
                "GET",
                vec![("contentRef", "canvas-file:canvas-1:deadbeef")],
                &[],
            ),
        );
        assert_eq!(get.status, "404 Not Found");
    }

    #[test]
    fn route_rejects_non_canvas_file_refs() {
        let app = credentialed_app();
        let get = canvas_file_response(
            &app,
            &request("GET", vec![("contentRef", "blob://other")], &[]),
        );
        assert_eq!(get.status, "400 Bad Request");
    }

    #[test]
    fn route_rejects_empty_body_and_artifact_id() {
        let app = credentialed_app();
        let empty_body = canvas_file_response(
            &app,
            &request(
                "PUT",
                vec![("artifactId", "canvas-1"), ("mimeType", "image/png")],
                &[],
            ),
        );
        assert_eq!(empty_body.status, "400 Bad Request");
        let empty_id = canvas_file_response(
            &app,
            &request(
                "PUT",
                vec![("artifactId", ""), ("mimeType", "image/png")],
                &png_bytes(),
            ),
        );
        assert_eq!(empty_id.status, "400 Bad Request");
    }

    #[test]
    fn route_rejects_unknown_media_types_and_mismatches() {
        let app = credentialed_app();
        let unknown = canvas_file_response(
            &app,
            &request(
                "PUT",
                vec![("artifactId", "canvas-1"), ("mimeType", "image/x-icon")],
                &png_bytes(),
            ),
        );
        assert_eq!(unknown.status, "415 Unsupported Media Type");
        let mismatch = canvas_file_response(
            &app,
            &request(
                "PUT",
                vec![("artifactId", "canvas-1"), ("mimeType", "image/png")],
                JPEG_HEADER,
            ),
        );
        assert_eq!(mismatch.status, "415 Unsupported Media Type");
    }

    #[test]
    fn route_requires_credentials() {
        let app = app();
        let mut unauthenticated = request(
            "PUT",
            vec![("artifactId", "canvas-1"), ("mimeType", "image/png")],
            &png_bytes(),
        );
        unauthenticated.authorization = None;
        assert_eq!(
            canvas_file_response(&app, &unauthenticated).status,
            "401 Unauthorized"
        );
    }
}
