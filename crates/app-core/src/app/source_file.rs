//! App-owned source-file read for the workspace UI.
//!
//! The semantic index stores element locators (`path`, `start_line`,
//! `end_line`) but no file text, and no plugin route serves file content. The
//! document view needs the text to show which knowledge came from which lines,
//! so the app serves it directly.
//!
//! Trust boundary: the requested path is canonicalized and must resolve inside
//! a canonicalized project root that the semantic index actually knows
//! (`SemanticOperation::ProjectRoots` — the same source of truth the semantic
//! plugin's `graph_project_roots` read uses). That defeats `..` traversal and
//! symlink escapes, because canonicalization resolves both before the
//! containment check.

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::AppCore;
use lumvise_db_core::{SemanticOperation, SemanticPersistence, SemanticResult};

mod intelligence;
pub(crate) use intelligence::invoke_project_source;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) const SOURCE_FILE_ENDPOINT: &str = "/api/source-file";

/// Largest text payload returned in one read; larger files are truncated.
const MAX_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn source_file_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        );
    }
    let Some(project_root) = non_empty(request, "project_root") else {
        return error_response("400 Bad Request", "project_root is required");
    };
    let Some(requested) = non_empty(request, "path") else {
        return error_response("400 Bad Request", "path is required");
    };

    let (canonical_root, canonical_file) = match indexed_source_path(app, &project_root, &requested)
    {
        Ok(file) => file,
        Err(failure) => return error_response(failure.status, failure.message),
    };
    if let Some(reference) = non_empty(request, "image") {
        return document_image_response(app, &canonical_file, &reference);
    }
    if request.query.get("format").map(String::as_str) == Some("binary") {
        return match read_binary_source(&canonical_file) {
            Ok(bytes) => HttpResponse::buffered(
                "200 OK",
                "application/octet-stream",
                bytes,
                vec![
                    ("X-Content-Type-Options".into(), "nosniff".into()),
                    ("Cache-Control".into(), "no-store".into()),
                ],
            ),
            Err(failure) => error_response(failure.status, failure.message),
        };
    }
    match converted_source_response(app, &canonical_root, &canonical_file) {
        Ok(Some(response)) => return response,
        Err(message) => return error_response("422 Unprocessable Entity", message),
        Ok(None) => {}
    }
    let metadata = match std::fs::metadata(&canonical_file) {
        Ok(metadata) => metadata,
        Err(error) => return error_response("404 Not Found", format!("{requested}: {error}")),
    };
    let byte_size = metadata.len();
    let truncated = byte_size > MAX_BYTES as u64;
    let mut buffer = Vec::with_capacity(byte_size.min(MAX_BYTES as u64) as usize);
    let read = File::open(&canonical_file)
        .and_then(|file| file.take(MAX_BYTES as u64).read_to_end(&mut buffer));
    if let Err(error) = read {
        return error_response(
            "500 Internal Server Error",
            format!("reading {requested} failed: {error}"),
        );
    }
    let Ok(text) = String::from_utf8(buffer) else {
        return error_response("400 Bad Request", format!("{requested} is not UTF-8 text"));
    };

    json_response(
        "200 OK",
        json!({
            "project_root": canonical_root.to_string_lossy(),
            "path": canonical_file.to_string_lossy(),
            "text": text,
            "truncated": truncated,
            "byte_size": byte_size,
        }),
    )
}

fn converted_source_response(
    app: &AppCore,
    root: &Path,
    file: &Path,
) -> Result<Option<HttpResponse>, String> {
    let (byte_size, converted) = read_document(app, file)?;
    Ok(converted.map(|document| json_response("200 OK", json!({
        "project_root":root.to_string_lossy(), "path":file.to_string_lossy(),
        "text":document.markdown, "truncated":false, "byte_size":byte_size, "representation":"markdown",
        "conversion":{"converter":document.provenance.converter,"enhancement":document.provenance.enhancement,"warnings":document.provenance.warnings},
        "images":document.provenance.figures.iter().map(|figure| json!({"reference":figure.reference,"caption":figure.caption,"page":figure.page})).collect::<Vec<_>>()
    }))))
}

fn read_document(
    app: &AppCore,
    file: &Path,
) -> Result<(usize, Option<lumvise_project_indexer::ConvertedDocument>), String> {
    // Only candidate documents need a complete read; ordinary text keeps its existing truncation limit.
    if !lumvise_project_indexer::is_document_path(&file.to_string_lossy()) {
        return Ok((0, None));
    }
    let bytes = std::fs::read(file)
        .map_err(|error| format!("{}: expected readable document: {error}", file.display()))?;
    let converted = super::document_conversion::converter(app)?
        .convert(&file.to_string_lossy(), &bytes)
        .map_err(|error| error.to_string())?;
    Ok((bytes.len(), converted))
}

fn document_image_response(app: &AppCore, file: &Path, reference: &str) -> HttpResponse {
    let image = match read_document(app, file) {
        Ok((_, document)) => document.and_then(|document| {
            document
                .images
                .into_iter()
                .find(|image| image.reference == reference)
        }),
        Err(error) => return error_response("422 Unprocessable Entity", error),
    };
    image.map_or_else(
        || {
            error_response(
                "404 Not Found",
                format!(
                    "image {reference}: expected an embedded image in {}",
                    file.display()
                ),
            )
        },
        embedded_image_response,
    )
}

fn embedded_image_response(image: lumvise_project_indexer::DocumentImage) -> HttpResponse {
    HttpResponse::buffered(
        "200 OK",
        image.media_type,
        image.bytes,
        vec![
            ("X-Content-Type-Options".into(), "nosniff".into()),
            ("Cache-Control".into(), "no-store".into()),
            (
                "Content-Security-Policy".into(),
                "sandbox; default-src 'none'".into(),
            ),
        ],
    )
}

/// Reads complete media bytes through the same indexed-root guard as text previews.
/// Example: `source_file_bytes(app, "/project", "images/photo.jpg")`.
#[cfg(any(feature = "desktop-app", test))]
pub(crate) fn source_file_bytes(app: &AppCore, root: &str, path: &str) -> Result<Vec<u8>, String> {
    indexed_source_path(app, root, path)
        .and_then(|(_, file)| read_binary_source(&file))
        .map_err(|failure| format!("{}: {}", failure.status, failure.message))
}

fn read_binary_source(file: &Path) -> Result<Vec<u8>, GuardFailure> {
    std::fs::read(file).map_err(|error| GuardFailure {
        status: "500 Internal Server Error",
        message: format!(
            "reading {} failed; expected a readable file: {error}",
            file.display()
        ),
    })
}

fn indexed_source_path(
    app: &AppCore,
    root: &str,
    path: &str,
) -> Result<(PathBuf, PathBuf), GuardFailure> {
    let canonical_root = indexed_root(
        app.semantic.as_ref(),
        root,
        &InvocationControl::sixty_seconds(),
    )?;
    let file = resolve_within_root(&canonical_root, path)?;
    if !file.is_file() {
        return Err(GuardFailure {
            status: "404 Not Found",
            message: format!("{path} is not a file"),
        });
    }
    Ok((canonical_root, file))
}

fn indexed_root(
    semantic: &dyn SemanticPersistence,
    root: &str,
    control: &InvocationControl,
) -> Result<PathBuf, GuardFailure> {
    let canonical_root = std::fs::canonicalize(root).map_err(|error| GuardFailure {
        status: "404 Not Found",
        message: format!("project_root {root} is unavailable: {error}"),
    })?;
    let roots = indexed_project_roots(semantic, control).map_err(|message| GuardFailure {
        status: "500 Internal Server Error",
        message,
    })?;
    if !roots
        .iter()
        .any(|entry| std::fs::canonicalize(entry).is_ok_and(|p| p == canonical_root))
    {
        return Err(GuardFailure {
            status: "404 Not Found",
            message: format!("project_root {root} is not an indexed project"),
        });
    }
    Ok(canonical_root)
}

fn non_empty(request: &HttpRequest, key: &str) -> Option<String> {
    request
        .query
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug)]
pub(crate) struct GuardFailure {
    status: &'static str,
    message: String,
}

/// Resolves `requested` (relative to `root`, or absolute) to a canonical file
/// strictly inside the canonical `root`. Canonicalizing first is what makes
/// `..` segments and symlinks out of the project unreachable.
fn resolve_within_root(root: &Path, requested: &str) -> Result<PathBuf, GuardFailure> {
    let candidate = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        root.join(requested)
    };
    let canonical = std::fs::canonicalize(&candidate).map_err(|error| GuardFailure {
        status: "404 Not Found",
        message: format!("{requested} is unavailable: {error}"),
    })?;
    if canonical == root || !canonical.starts_with(root) {
        return Err(GuardFailure {
            status: "400 Bad Request",
            message: format!("{requested} resolves outside the project root"),
        });
    }
    Ok(canonical)
}

fn indexed_project_roots(
    semantic: &dyn SemanticPersistence,
    control: &InvocationControl,
) -> Result<Vec<String>, String> {
    match semantic.execute(SemanticOperation::ProjectRoots, control) {
        Ok(SemanticResult::ProjectRoots(roots)) => Ok(roots),
        Ok(other) => Err(format!(
            "unexpected semantic result for project roots: {other:?}"
        )),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write;

    fn request(project_root: &str, path: &str) -> HttpRequest {
        let mut query = BTreeMap::new();
        query.insert("project_root".to_string(), project_root.to_string());
        query.insert("path".to_string(), path.to_string());
        HttpRequest {
            method: "GET".to_string(),
            path: SOURCE_FILE_ENDPOINT.to_string(),
            query,
            authorization: Some("Bearer source-file-test".to_string()),
            body: Vec::new(),
        }
    }

    fn credentialed_app() -> AppCore {
        let app = AppCore::in_memory().expect("in-memory app");
        app.install_bridge_credential_store(std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )))
        .expect("credential store");
        app.set_bridge_credential(Some("source-file-test".into()), u64::MAX)
            .expect("credential");
        app
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("lumvise-source-file-{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("scratch directory");
        directory
    }

    fn index_root(app: &AppCore, root: &Path) {
        let project_root = root.to_string_lossy().into_owned();
        let element = lumvise_db_core::SemanticElement {
            project_root: project_root.clone(),
            semantic_element_id: "media-root".into(),
            semantic_source_id: "filesystem".into(),
            path: ".".into(),
            element_kind: "folder".into(),
            name: "Media".into(),
            parent_element_id: None,
            content_fingerprint: None,
            start_line: None,
            end_line: None,
            lifecycle: "active".into(),
            match_evidence: None,
            metadata: json!({}),
        };
        app.semantic
            .execute(
                SemanticOperation::SyncStructure {
                    project_root,
                    elements: vec![element],
                    relationships: vec![],
                },
                &InvocationControl::sixty_seconds(),
            )
            .expect("index project");
    }

    #[test]
    fn binary_route_and_desktop_read_preserve_complete_non_utf8_bytes() {
        let app = std::sync::Arc::new(credentialed_app());
        let directory = scratch_dir("binary-complete");
        index_root(&app, &directory);
        let bytes = vec![0xff; MAX_BYTES + 100];
        std::fs::write(directory.join("sample.mp4"), &bytes).unwrap();
        let mut request = request(&directory.to_string_lossy(), "sample.mp4");
        request.query.insert("format".into(), "binary".into());
        let response = source_file_response(&app, &request);
        assert_eq!(response.status, "200 OK");
        assert_eq!(response.buffered_bytes().unwrap(), bytes);
        assert_eq!(
            source_file_bytes(&app, &directory.to_string_lossy(), "sample.mp4").unwrap(),
            bytes
        );
        #[cfg(feature = "desktop-app")]
        {
            use lumvise_frontend_core::DesktopSemanticGraphBridge;
            let bridge = crate::app::desktop::AppCoreDesktopBridge::without_runtime(app.clone());
            assert_eq!(
                bridge
                    .read_source_file_bytes(
                        directory.to_string_lossy().into_owned(),
                        "sample.mp4".into()
                    )
                    .unwrap(),
                bytes
            );
        }
        request.query.remove("format");
        assert_eq!(
            source_file_response(&app, &request).status,
            "400 Bad Request"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn document_text_route_uses_the_same_markdown_as_the_indexer() {
        let app = credentialed_app();
        let directory = scratch_dir("converted-document");
        index_root(&app, &directory);
        let bytes = b"<h1>Coastal report</h1><p>Calibrate monthly.</p>";
        std::fs::write(directory.join("report.html"), bytes).unwrap();
        let response =
            source_file_response(&app, &request(&directory.to_string_lossy(), "report.html"));
        assert_eq!(response.status, "200 OK");
        let payload: serde_json::Value =
            serde_json::from_slice(response.buffered_bytes().unwrap()).unwrap();
        let converted = lumvise_project_indexer::convert_document("report.html", bytes)
            .unwrap()
            .unwrap();
        assert_eq!(payload["text"], converted.markdown);
        assert_eq!(payload["representation"], "markdown");
        std::fs::write(directory.join("broken.pdf"), b"%PDF-invalid").unwrap();
        assert_eq!(
            source_file_response(&app, &request(&directory.to_string_lossy(), "broken.pdf")).status,
            "422 Unprocessable Entity"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn binary_reads_require_credentials_and_an_indexed_containing_root() {
        let app = credentialed_app();
        let directory = scratch_dir("binary-guards");
        let outside = scratch_dir("binary-outside");
        std::fs::write(outside.join("private.pdf"), b"private").unwrap();
        let mut request = request(&directory.to_string_lossy(), "missing.pdf");
        request.query.insert("format".into(), "binary".into());
        assert_eq!(source_file_response(&app, &request).status, "404 Not Found");
        assert!(
            source_file_bytes(&app, &directory.to_string_lossy(), "missing.pdf")
                .unwrap_err()
                .contains("not an indexed project")
        );
        index_root(&app, &directory);
        let outside_path = outside.join("private.pdf").to_string_lossy().into_owned();
        request.query.insert("path".into(), outside_path.clone());
        assert_eq!(
            source_file_response(&app, &request).status,
            "400 Bad Request"
        );
        assert!(
            source_file_bytes(&app, &directory.to_string_lossy(), &outside_path)
                .unwrap_err()
                .contains("outside the project root")
        );
        request.authorization = None;
        assert_eq!(
            source_file_response(&app, &request).status,
            "401 Unauthorized"
        );
        std::fs::remove_dir_all(directory).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn document_images_are_served_as_binary_behind_the_existing_source_guard() {
        let app = credentialed_app();
        let directory = tempfile::tempdir().unwrap();
        index_root(&app, directory.path());
        let bytes =
            include_bytes!("../../../project-indexer/tests/fixtures/documents/illustrated.docx");
        std::fs::write(directory.path().join("report.docx"), bytes).unwrap();
        let mut request = request(&directory.path().to_string_lossy(), "report.docx");
        let metadata = source_file_response(&app, &request);
        assert_eq!(metadata.status, "200 OK");
        let payload: serde_json::Value =
            serde_json::from_slice(metadata.buffered_bytes().unwrap()).unwrap();
        let reference = payload["images"][0]["reference"].as_str().unwrap();
        assert!(!payload.to_string().contains("base64"));
        request.query.insert("image".into(), reference.into());
        let image = source_file_response(&app, &request);
        assert_eq!(image.status, "200 OK");
        assert_eq!(image.content_type, "image/png");
        assert_eq!(
            image.buffered_bytes().unwrap(),
            include_bytes!("../../../project-indexer/tests/fixtures/gradient.png")
        );
        request
            .query
            .insert("image".into(), "../../private.png".into());
        assert_eq!(source_file_response(&app, &request).status, "404 Not Found");
        request.authorization = None;
        assert_eq!(
            source_file_response(&app, &request).status,
            "401 Unauthorized"
        );
    }

    #[test]
    fn preview_uses_the_runtime_document_opt_out() {
        let app = credentialed_app();
        app.frontend()
            .apply_app_settings_patch(
                &lumvise_frontend_core::AppSettingsPatch::DocumentEnhancementEnabled(false),
            )
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        index_root(&app, directory.path());
        let bytes = include_bytes!("../../../project-indexer/tests/fixtures/documents/report.pdf");
        std::fs::write(directory.path().join("report.pdf"), bytes).unwrap();
        let preview = source_file_response(
            &app,
            &request(&directory.path().to_string_lossy(), "report.pdf"),
        );
        let payload: serde_json::Value =
            serde_json::from_slice(preview.buffered_bytes().unwrap()).unwrap();
        let converted = super::super::document_conversion::converter(&app)
            .unwrap()
            .convert("report.pdf", bytes)
            .unwrap()
            .unwrap();
        assert_eq!(payload["text"], converted.markdown);
        assert_eq!(payload["conversion"]["enhancement"], "disabled");
    }

    #[test]
    fn rejects_missing_credential() {
        let app = credentialed_app();
        let mut unauthenticated = request("/tmp", "file.txt");
        unauthenticated.authorization = None;
        assert_eq!(
            source_file_response(&app, &unauthenticated).status,
            "401 Unauthorized"
        );
    }

    #[test]
    fn rejects_unknown_project_root() {
        let app = credentialed_app();
        let directory = scratch_dir("unknown-root");
        let file = directory.join("file.txt");
        std::fs::write(&file, b"contents").expect("write file");
        let response = source_file_response(
            &app,
            &request(&directory.to_string_lossy(), &file.to_string_lossy()),
        );
        // No project is indexed in an in-memory app, so every root is unknown.
        assert_eq!(response.status, "404 Not Found");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn accepts_a_file_inside_the_root() {
        let directory = scratch_dir("inside");
        let file = directory.join("nested/file.txt");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("nested directory");
        std::fs::write(&file, b"contents").expect("write file");
        let root = std::fs::canonicalize(&directory).expect("canonical root");

        let resolved = resolve_within_root(&root, "nested/file.txt").expect("inside root");
        assert_eq!(
            resolved,
            std::fs::canonicalize(&file).expect("canonical file")
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_parent_traversal() {
        let directory = scratch_dir("traversal");
        let root = directory.join("project");
        std::fs::create_dir_all(&root).expect("project root");
        std::fs::write(directory.join("secret.txt"), b"secret").expect("write outside file");
        let canonical_root = std::fs::canonicalize(&root).expect("canonical root");

        let failure = resolve_within_root(&canonical_root, "../secret.txt")
            .expect_err("traversal must be rejected");
        assert_eq!(failure.status, "400 Bad Request");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let directory = scratch_dir("symlink");
        let root = directory.join("project");
        std::fs::create_dir_all(&root).expect("project root");
        let outside = directory.join("secret.txt");
        std::fs::write(&outside, b"secret").expect("write outside file");
        std::os::unix::fs::symlink(&outside, root.join("escape.txt")).expect("symlink");
        let canonical_root = std::fs::canonicalize(&root).expect("canonical root");

        let failure = resolve_within_root(&canonical_root, "escape.txt")
            .expect_err("symlink escape must be rejected");
        assert_eq!(failure.status, "400 Bad Request");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn reports_missing_files_as_not_found() {
        let directory = scratch_dir("missing");
        let root = std::fs::canonicalize(&directory).expect("canonical root");
        let failure =
            resolve_within_root(&root, "absent.txt").expect_err("missing file must be rejected");
        assert_eq!(failure.status, "404 Not Found");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
