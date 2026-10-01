//! App-owned project import for the Knowledge Canvas UI.
//!
//! The renderer's folder import previously went through the `builtin.canvas`
//! plugin's `import_folder` export. That still exists (it is the right home
//! for sandbox-permitted paths and is tested), but the plugin runs in a child
//! process that has no macOS TCC grant for the user's Documents/Desktop
//! folders, while the app process does. The UI therefore calls this
//! app-owned route, which scans with [`lumvise_project_indexer`] in the app
//! process and publishes the projection through the semantic plugin's
//! `ingest_index_batch` MCP tool — the same staged-page scheme the plugin
//! module uses (`crates/plugin/builtins/canvas/src/import.rs`).
//!
//! Threading: every app-owned route handler already runs inside
//! `tokio::task::spawn_blocking` (`mcp_http.rs`), so the scan — which can
//! take seconds — blocks a dedicated worker thread, never an async runtime
//! worker. Each ingestion page stays far below the plugin invocation
//! deadline (1000 elements / 5000 relationships per page).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use lumvise_contracts::IndexBatchRequest;
use lumvise_db_core::{SemanticOperation, SemanticResult};
use lumvise_project_indexer::{
    FilesystemProjectSource, ParallelTreeSitterProjectParser, PreparedProjectScan, ProjectIndexer,
    ScanError, ScanScope, SemanticIndexProjection, SourceKind,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::{AppCore, PluginInvocationRequest, PluginInvocationStatus};

pub(crate) const PROJECT_IMPORT_ENDPOINT: &str = "/api/project-import";

const SEMANTIC_PLUGIN_ID: &str = "builtin.semantic";
const INGEST_EXPORT_ID: &str = "ingest_index_batch";
const RECORD_INDEX_LOG_EXPORT_ID: &str = "record_index_log";
/// Projection identity: the same provider instance the plugin import writes,
/// so scoped reads and `list_projects` see one project regardless of which
/// importer ran.
const PROVIDER_INSTANCE_ID: &str = "builtin-canvas";
/// Names the app-owned importer in index-log rows (the plugin import writes
/// `builtin.canvas`).
const IMPORTER_PLUGIN_ID: &str = "app.project-import";
/// Names the app-owned PZ snapshot importer in index-log rows; the refresh
/// poller skips projects whose latest import came from this importer.
const PZ_IMPORTER_PLUGIN_ID: &str = "app.pz-import";
/// Elements per staged ingestion page; keeps one `ingest_index_batch` call
/// (validation + source mutations + one staging write) far below the
/// invocation deadline. Relationships share the page schedule at a larger
/// stride, so pages = max(element pages, relationship pages).
const ELEMENTS_PER_PAGE: usize = 1000;
const RELATIONSHIPS_PER_PAGE: usize = 5000;
/// Upper bound for the skip report so one huge ignored tree cannot bloat the
/// response; the walk stops expanding after this many exclusion points.
const MAX_SKIP_REPORTS: usize = 500;

/// Parses `{folderPath, projectRoot?, replacePaths?, source?}` (camelCase,
/// renderer contract); `source` selects the disk scan (`None`/`"folder"`) or
/// the PZ snapshot import (`"pz"`).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportInput {
    folder_path: String,
    project_root: Option<String>,
    replace_paths: Option<Vec<String>>,
    source: Option<String>,
}

pub(crate) type ProjectImportSessions = BTreeMap<(PathBuf, String), ProjectImportSession>;

pub(crate) struct ProjectImportSession {
    indexer: ProjectIndexer<FilesystemProjectSource, ParallelTreeSitterProjectParser>,
    projection: SemanticIndexProjection,
}

fn import_session(root: &Path) -> Result<ProjectImportSession, HttpResponse> {
    Ok(ProjectImportSession {
        indexer: ProjectIndexer::new(
            FilesystemProjectSource::open(root).map_err(scan_failure)?,
            ParallelTreeSitterProjectParser::new(parser_workers()).map_err(scan_failure)?,
        ),
        projection: SemanticIndexProjection::new(root, PROVIDER_INSTANCE_ID)
            .map_err(scan_failure)?,
    })
}

/// Polls local indexed projects through the same serialized importer as the UI.
pub(super) fn refresh_local_projects(app: &AppCore) -> Result<(), String> {
    let response = app
        .plugin_endpoints()
        .invoke_plugin_mcp_tool(PluginInvocationRequest {
            tool_name: "app_plugin.builtin.canvas.list_projects".into(),
            arguments: json!({}),
        })
        .map_err(|error| error.to_string())?;
    let projects = response.output["projects"].as_array().ok_or_else(|| {
        format!(
            "invalid project catalog `{}`; expected projects array",
            response.output
        )
    })?;
    for project in projects {
        let Some(root) = project["projectRoot"]
            .as_str()
            .filter(|root| Path::new(root).is_dir())
        else {
            continue;
        };
        if let Ok(log) = semantic(app, "latest_index_log", json!({"project_root": root})) {
            // A PZ import restores the structure from the archive; demo trees
            // hold only `.lv/graph.pz` on disk, so a rescan would wipe it.
            // A failed log read keeps the existing rescan behaviour.
            if log["index_log"]["plugin_id"] == PZ_IMPORTER_PLUGIN_ID {
                continue;
            }
        }
        if let Err(response) = import(
            app,
            ImportInput {
                folder_path: root.into(),
                project_root: Some(root.into()),
                replace_paths: None,
                source: None,
            },
        ) {
            tracing::error!(event = "project_refresh_failed", project_root = root,
                error = %String::from_utf8_lossy(response.buffered_bytes().unwrap_or_default()));
        }
    }
    Ok(())
}

pub(crate) fn project_import_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        );
    }
    let input = match serde_json::from_slice::<ImportInput>(&request.body) {
        Ok(input) => input,
        Err(error) => {
            return error_response(
                "400 Bad Request",
                format!(
                    "invalid request body; expected object `{{folderPath, projectRoot?, replacePaths?}}`: {error}"
                ),
            );
        }
    };
    match input.source.as_deref() {
        None | Some("folder") => match import(app, input) {
            Ok(response) => json_response("200 OK", response),
            Err(response) => response,
        },
        Some("pz") => match import_pz(app, input) {
            Ok(response) => json_response("200 OK", response),
            Err(response) => response,
        },
        Some(other) => error_response(
            "400 Bad Request",
            format!("invalid import source `{other}`; expected \"folder\" or \"pz\""),
        ),
    }
}

/// Scans one folder into the semantic database and reports the import result.
fn import(app: &AppCore, input: ImportInput) -> Result<Value, HttpResponse> {
    let started_at = Utc::now().to_rfc3339();
    let root = canonical_root(&input.folder_path)?;
    let project_root = input
        .project_root
        .unwrap_or_else(|| root.to_string_lossy().into_owned());

    // Keep scan acknowledgements across refreshes. Serialize publication with
    // manual imports so a delayed batch cannot overwrite a newer source state.
    let mut sessions = app.project_import_sessions.lock().map_err(|_| {
        error_response(
            "500 Internal Server Error",
            "project import sessions poisoned; expected available importer",
        )
    })?;
    let session = match sessions.entry((root.clone(), project_root.clone())) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::btree_map::Entry::Vacant(entry) => entry.insert(import_session(&root)?),
    };
    let ProjectImportSession {
        indexer,
        projection,
    } = session;
    let scope = match &input.replace_paths {
        Some(paths) if !paths.is_empty() => ScanScope::Paths(paths.clone()),
        _ => ScanScope::Full,
    };
    let scan = indexer.prepare(scope).map_err(scan_failure)?;
    let full_snapshot = scan.is_full_snapshot();
    let files_scanned = scan
        .changed_files()
        .filter(|file| file.entry.kind == SourceKind::File)
        .count();

    if !scan.needs_publication() {
        indexer.commit(scan).map_err(scan_failure)?;
        return Ok(finished(&project_root, 0, 0, 0, Vec::new(), &started_at));
    }

    let mut batch = projection.project(&scan).map_err(scan_failure)?;
    let skipped = if full_snapshot {
        collect_excluded(&root, &accepted_paths(&scan))
    } else {
        // A selective import replaces exactly the requested paths; policy
        // exclusions outside that scope are not part of this import.
        Vec::new()
    };
    // The caller-supplied identity wins so scoped reads later match the
    // returned `projectRoot`; the source upsert keeps the real folder path.
    batch.project_root = project_root.clone();
    let elements_upserted = batch.semantic_elements.len();
    let relationships_upserted = batch.semantic_relationships.len();

    let job_id = format!("app-project-import-{}", Utc::now().timestamp_millis());
    let pages = snapshot_pages(&batch, &job_id, full_snapshot);
    let page_count = pages.len();
    let source_id = batch
        .semantic_sources
        .first()
        .map(|source| source.semantic_source_id.clone());

    // Best-effort start marker so the UI can observe an in-flight import.
    let _ = record_index_log(
        app,
        json!({
            "provider_instance_id": PROVIDER_INSTANCE_ID,
            "plugin_id": IMPORTER_PLUGIN_ID,
            "semantic_source_id": source_id,
            "project_root": project_root,
            "index_log_id": job_id,
            "status": "running",
            "metrics": {
                "filesScanned": files_scanned,
                "elementsUpserted": elements_upserted,
                "relationshipsUpserted": relationships_upserted,
                "skipped": skipped.len(),
                "pageCount": page_count,
            },
            "started_at": started_at,
        }),
    );

    if let Err(error) = publish_pages(app, &pages) {
        // Terminal state record; the staging area itself expires via TTL.
        let _ = record_index_log(
            app,
            json!({
                "provider_instance_id": PROVIDER_INSTANCE_ID,
                "plugin_id": IMPORTER_PLUGIN_ID,
                "project_root": project_root,
                "index_log_id": job_id,
                "status": "failed",
                "metrics": {
                    "filesScanned": files_scanned,
                    "pageCount": page_count,
                },
                "error": error,
                "started_at": started_at,
                "completed_at": Utc::now().to_rfc3339(),
            }),
        );
        return Err(error_response(
            "500 Internal Server Error",
            format!("project import failed: {error}"),
        ));
    }
    indexer.commit(scan).map_err(scan_failure)?;

    record_index_log(
        app,
        json!({
            "provider_instance_id": PROVIDER_INSTANCE_ID,
            "plugin_id": IMPORTER_PLUGIN_ID,
            "project_root": project_root,
            "index_log_id": job_id,
            "status": "completed",
            "metrics": {
                "filesScanned": files_scanned,
                "elementsUpserted": elements_upserted,
                "relationshipsUpserted": relationships_upserted,
                "skipped": skipped.len(),
                "pageCount": page_count,
            },
            "started_at": started_at,
            "completed_at": Utc::now().to_rfc3339(),
        }),
    )
    .map_err(|error| {
        error_response(
            "500 Internal Server Error",
            format!("project import published but the completion log failed: {error}"),
        )
    })?;

    Ok(finished(
        &project_root,
        files_scanned,
        elements_upserted,
        relationships_upserted,
        skipped,
        &started_at,
    ))
}

/// Imports one project from its `.lv/graph.pz` snapshot archive. The archive
/// is the only on-disk content of demo projects, so the import restores the
/// structure through `ImportPzSnapshot` instead of a disk scan; the refresh
/// poller skips these projects (see [`refresh_local_projects`]).
fn import_pz(app: &AppCore, input: ImportInput) -> Result<Value, HttpResponse> {
    let started_at = Utc::now().to_rfc3339();
    let root = canonical_root(&input.folder_path)?;
    let project_root = input
        .project_root
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let input_path = root.join(".lv").join("graph.pz");
    if !input_path.is_file() {
        return Err(error_response(
            "404 Not Found",
            format!(
                "project has no linked snapshot at {}; expected .lv/graph.pz",
                input_path.to_string_lossy()
            ),
        ));
    }

    // Serialize with disk imports and the refresh poller, and drop any
    // retained scan session so a later disk rescan starts from scratch.
    let mut sessions = app.project_import_sessions.lock().map_err(|_| {
        error_response(
            "500 Internal Server Error",
            "project import sessions poisoned; expected available importer",
        )
    })?;
    sessions.remove(&(root.clone(), project_root.clone()));

    let job_id = format!("app-pz-import-{}", Utc::now().timestamp_millis());
    // Best-effort start marker so the UI can observe an in-flight import.
    let _ = record_index_log(
        app,
        json!({
            "provider_instance_id": PROVIDER_INSTANCE_ID,
            "plugin_id": PZ_IMPORTER_PLUGIN_ID,
            "project_root": project_root,
            "index_log_id": job_id,
            "status": "running",
            "metrics": {
                "elementsUpserted": 0,
                "relationshipsUpserted": 0,
                "elementsMarkedInactive": 0,
                "artifactsImported": 0,
                "artifactsKept": 0,
            },
            "started_at": started_at,
        }),
    );

    let result = match app.semantic.execute(
        SemanticOperation::ImportPzSnapshot {
            project_root: project_root.clone(),
            input_path: input_path.to_string_lossy().into_owned(),
        },
        &InvocationControl::sixty_seconds(),
    ) {
        Ok(SemanticResult::PzImport(result)) => result,
        Ok(other) => {
            return Err(error_response(
                "500 Internal Server Error",
                format!("project import failed: unexpected PZ import result {other:?}"),
            ));
        }
        Err(error) => {
            // Best-effort terminal state record; DB Core left the project unchanged.
            let _ = record_index_log(
                app,
                json!({
                    "provider_instance_id": PROVIDER_INSTANCE_ID,
                    "plugin_id": PZ_IMPORTER_PLUGIN_ID,
                    "project_root": project_root,
                    "index_log_id": job_id,
                    "status": "failed",
                    "metrics": {},
                    "error": error.to_string(),
                    "started_at": started_at,
                    "completed_at": Utc::now().to_rfc3339(),
                }),
            );
            return Err(error_response(
                "500 Internal Server Error",
                format!("project import failed: {error}"),
            ));
        }
    };
    let structure = &result.structure;

    record_index_log(
        app,
        json!({
            "provider_instance_id": PROVIDER_INSTANCE_ID,
            "plugin_id": PZ_IMPORTER_PLUGIN_ID,
            "project_root": project_root,
            "index_log_id": job_id,
            "status": "completed",
            "metrics": {
                "elementsUpserted": structure.elements_upserted,
                "relationshipsUpserted": structure.relationships_upserted,
                "elementsMarkedInactive": structure.elements_marked_inactive,
                "artifactsImported": result.artifacts_imported,
                "artifactsKept": result.artifacts_kept,
            },
            "started_at": started_at,
            "completed_at": Utc::now().to_rfc3339(),
        }),
    )
    .map_err(|error| {
        error_response(
            "500 Internal Server Error",
            format!("project import published but the completion log failed: {error}"),
        )
    })?;

    let mut response = finished(
        &project_root,
        0,
        structure.elements_upserted,
        structure.relationships_upserted,
        Vec::new(),
        &started_at,
    );
    let object = response
        .as_object_mut()
        .expect("import result is an object");
    object.insert("source".into(), json!("pz"));
    object.insert("artifactsImported".into(), json!(result.artifacts_imported));
    object.insert("artifactsKept".into(), json!(result.artifacts_kept));
    Ok(response)
}

/// Splits one projection batch into staged ingestion pages. Page 0 carries the
/// source upsert and the complete replacement partition (`replace_paths` +
/// `removed_paths`) because the host takes the partition from the first page;
/// a full snapshot deliberately keeps `replace_paths` empty so the commit
/// replaces the whole project structure. Later pages carry the paths their
/// elements cover (partial imports only).
fn snapshot_pages(batch: &IndexBatchRequest, job_id: &str, full_snapshot: bool) -> Vec<Value> {
    let elements = batch.semantic_elements.len();
    let relationships = batch.semantic_relationships.len();
    let page_count = elements
        .div_ceil(ELEMENTS_PER_PAGE)
        .max(relationships.div_ceil(RELATIONSHIPS_PER_PAGE))
        .max(1);
    (0..page_count)
        .map(|index| {
            let element_window = window(index, ELEMENTS_PER_PAGE, elements);
            let relationship_window = window(index, RELATIONSHIPS_PER_PAGE, relationships);
            let mut page = serde_json::to_value(batch).expect("index batch serializes");
            let object = page.as_object_mut().expect("index batch is an object");
            if index == 0 {
                object.insert("removed_paths".into(), json!(batch.removed_paths));
            } else {
                object.insert("removed_paths".into(), json!([]));
            }
            object.insert(
                "semantic_sources".into(),
                if index == 0 {
                    json!(batch.semantic_sources)
                } else {
                    json!([])
                },
            );
            let replace_paths = if full_snapshot {
                json!([])
            } else if index == 0 {
                json!(batch.replace_paths)
            } else {
                let covered: BTreeSet<&String> = batch.semantic_elements
                    [element_window.0..element_window.1]
                    .iter()
                    .map(|element| &element.path)
                    .collect();
                json!(covered)
            };
            object.insert("replace_paths".into(), replace_paths);
            object.insert(
                "semantic_elements".into(),
                json!(batch.semantic_elements[element_window.0..element_window.1]),
            );
            object.insert(
                "semantic_relationships".into(),
                json!(batch.semantic_relationships[relationship_window.0..relationship_window.1]),
            );
            object.insert("ingestion_job_id".into(), json!(job_id));
            object.insert("ingestion_page_index".into(), json!(index));
            object.insert("ingestion_page_count".into(), json!(page_count));
            page
        })
        .collect()
}

/// Half-open `[start, end)` window for one page, clamped to `total`.
fn window(index: usize, per_page: usize, total: usize) -> (usize, usize) {
    let start = (index * per_page).min(total);
    let end = ((index + 1) * per_page).min(total);
    (start, end.max(start))
}

/// Publishes every staged page through the semantic plugin's
/// `ingest_index_batch` MCP tool.
fn publish_pages(app: &AppCore, pages: &[Value]) -> Result<(), String> {
    for page in pages {
        let response = semantic(app, INGEST_EXPORT_ID, page.clone())?;
        if response["accepted"] != json!(true) {
            return Err(format!(
                "semantic ingestion rejected an import page: {response}"
            ));
        }
    }
    Ok(())
}

/// Validates and canonicalizes the requested folder.
fn canonical_root(folder_path: &str) -> Result<PathBuf, HttpResponse> {
    if folder_path.trim().is_empty() {
        return Err(error_response("400 Bad Request", "folderPath is required"));
    }
    let path = PathBuf::from(folder_path);
    if !path.exists() {
        return Err(error_response(
            "400 Bad Request",
            format!("folder `{folder_path}` does not exist"),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        error_response(
            "400 Bad Request",
            format!("cannot canonicalize folder `{folder_path}`: {error}"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(error_response(
            "400 Bad Request",
            format!("`{folder_path}` is not a directory"),
        ));
    }
    Ok(canonical)
}

/// Accepted inventory paths from one prepared scan.
fn accepted_paths(scan: &PreparedProjectScan) -> BTreeSet<String> {
    scan.changed_files()
        .map(|file| file.entry.path.clone())
        .collect()
}

/// Walks the folder once and reports the exclusion points the scanner applied:
/// every path that is absent from the accepted inventory, without descending
/// into excluded subtrees (one entry per exclusion gate, not per file).
fn collect_excluded(root: &Path, accepted: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut skipped = Vec::new();
    walk_excluded(root, "", accepted, &mut skipped);
    skipped
}

fn walk_excluded(
    root: &Path,
    relative: &str,
    accepted: &BTreeSet<String>,
    skipped: &mut Vec<(String, String)>,
) {
    if skipped.len() >= MAX_SKIP_REPORTS {
        return;
    }
    let directory = if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let metadata = match entry.path().symlink_metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !accepted.contains(&child) {
            let reason = if metadata.is_symlink() {
                "symlink (not followed)"
            } else if !metadata.is_dir() && !metadata.is_file() {
                "unsupported file type"
            } else {
                "excluded by ignore rules (project ignore files or scanner defaults)"
            };
            skipped.push((child, reason.to_owned()));
            if skipped.len() >= MAX_SKIP_REPORTS {
                return;
            }
            continue;
        }
        if metadata.is_dir() {
            walk_excluded(root, &child, accepted, skipped);
            if skipped.len() >= MAX_SKIP_REPORTS {
                return;
            }
        }
    }
}

/// Bounded syntax worker pool for the in-process scan.
fn parser_workers() -> usize {
    std::thread::available_parallelism()
        .map(|workers| workers.get().min(4))
        .unwrap_or(2)
}

fn finished(
    project_root: &str,
    files_scanned: usize,
    elements_upserted: usize,
    relationships_upserted: usize,
    skipped: Vec<(String, String)>,
    started_at: &str,
) -> Value {
    json!({
        "projectRoot": project_root,
        "filesScanned": files_scanned,
        "elementsUpserted": elements_upserted,
        "relationshipsUpserted": relationships_upserted,
        "skipped": skipped
            .into_iter()
            .map(|(path, reason)| json!({"path": path, "reason": reason}))
            .collect::<Vec<_>>(),
        "startedAt": started_at,
        "finishedAt": Utc::now().to_rfc3339(),
    })
}

/// Invokes one `builtin.semantic` MCP export through the app's plugin
/// endpoints (in-process dispatch into the compiled plugin, no HTTP hop).
fn semantic(app: &AppCore, export_id: &str, input: Value) -> Result<Value, String> {
    let response = app
        .plugin_endpoints()
        .invoke_plugin_mcp_tool(PluginInvocationRequest {
            tool_name: format!("app_plugin.{SEMANTIC_PLUGIN_ID}.{export_id}"),
            arguments: input,
        })
        .map_err(|error| format!("builtin.semantic/{export_id} unavailable: {error}"))?;
    match response.status {
        PluginInvocationStatus::Completed | PluginInvocationStatus::Accepted => Ok(response.output),
        PluginInvocationStatus::Failed => Err(format!(
            "builtin.semantic/{export_id} failed: {}",
            response.output
        )),
    }
}

fn record_index_log(app: &AppCore, entry: Value) -> Result<Value, String> {
    semantic(app, RECORD_INDEX_LOG_EXPORT_ID, entry)
}

fn scan_failure(error: ScanError) -> HttpResponse {
    error_response("500 Internal Server Error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppCore;
    use std::fs;
    use tempfile::TempDir;

    fn credentialed_app() -> AppCore {
        let app = AppCore::in_memory().expect("in-memory app");
        app.install_bridge_credential_store(std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )))
        .expect("credential store");
        app.set_bridge_credential(Some("project-import-test".into()), u64::MAX)
            .expect("credential");
        app
    }

    fn request(body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: PROJECT_IMPORT_ENDPOINT.to_string(),
            query: std::collections::BTreeMap::new(),
            authorization: Some("Bearer project-import-test".to_string()),
            body: serde_json::to_vec(&body).expect("body serializes"),
        }
    }

    fn response_body(response: &HttpResponse) -> Value {
        serde_json::from_slice(response.buffered_bytes().expect("buffered body"))
            .expect("response body is JSON")
    }

    /// Fixture tree: one `.rs`, one root `.md`, a gitignored file, and the
    /// `.gitignore` naming it.
    fn fixture_tree() -> TempDir {
        let directory = TempDir::new().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    helper();\n}\n\nfn helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("README.md"), "# Guide\n").unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.join("ignored.txt"), "hidden\n").unwrap();
        directory
    }

    #[test]
    fn rejects_missing_folder_path() {
        let app = credentialed_app();
        for body in [
            json!({}),
            json!({"folderPath": "   "}),
            json!({"folderPath": 7}),
        ] {
            let response = project_import_response(&app, &request(body));
            assert_eq!(response.status, "400 Bad Request");
        }
    }

    #[test]
    fn rejects_file_instead_of_directory() {
        let app = credentialed_app();
        let directory = TempDir::new().unwrap();
        let file = directory.path().join("note.txt");
        fs::write(&file, "x").unwrap();
        let response = project_import_response(
            &app,
            &request(json!({"folderPath": file.to_string_lossy()})),
        );
        assert_eq!(response.status, "400 Bad Request");
        let body = response_body(&response);
        assert!(body.to_string().contains("is not a directory"));
    }

    #[test]
    fn rejects_missing_credential() {
        let app = credentialed_app();
        let mut unauthenticated = request(json!({"folderPath": "/tmp"}));
        unauthenticated.authorization = None;
        assert_eq!(
            project_import_response(&app, &unauthenticated).status,
            "401 Unauthorized"
        );
    }

    #[test]
    fn scans_fixture_and_names_ignored_paths() {
        let directory = fixture_tree();
        let root = directory.path().canonicalize().unwrap();
        let mut indexer = ProjectIndexer::new(
            FilesystemProjectSource::open(&root).unwrap(),
            ParallelTreeSitterProjectParser::new(2).unwrap(),
        );
        let scan = indexer.prepare(ScanScope::Full).unwrap();
        assert!(scan.is_full_snapshot());
        let files_scanned = scan
            .changed_files()
            .filter(|file| file.entry.kind == SourceKind::File)
            .count();
        // src/main.rs, README.md, .gitignore — `ignored.txt` is excluded.
        assert_eq!(files_scanned, 3);

        let mut projection = SemanticIndexProjection::new(&root, PROVIDER_INSTANCE_ID).unwrap();
        let batch = projection.project(&scan).unwrap();
        let elements_upserted = batch.semantic_elements.len();
        assert!(elements_upserted > 0);
        assert!(batch.semantic_elements.iter().any(|element| {
            element.path == "src/main.rs" && element.semantic_element_type == "file"
        }));
        assert!(
            batch
                .semantic_elements
                .iter()
                .any(|element| element.path == "README.md")
        );

        let skipped = collect_excluded(&root, &accepted_paths(&scan));
        let skipped_paths: BTreeSet<&str> = skipped.iter().map(|(path, _)| path.as_str()).collect();
        assert!(skipped_paths.contains("ignored.txt"));
        assert!(!skipped_paths.contains("src/main.rs"));

        // The route's response numbers come from exactly these values.
        let result = finished(
            root.to_string_lossy().as_ref(),
            files_scanned,
            elements_upserted,
            batch.semantic_relationships.len(),
            skipped,
            "started",
        );
        assert_eq!(result["filesScanned"], json!(3));
        assert_eq!(result["elementsUpserted"], json!(elements_upserted));
        let skipped = result["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["path"], json!("ignored.txt"));
    }

    #[test]
    fn full_snapshot_pages_carry_no_replace_paths() {
        let directory = fixture_tree();
        let root = directory.path().canonicalize().unwrap();
        let mut indexer = ProjectIndexer::new(
            FilesystemProjectSource::open(&root).unwrap(),
            ParallelTreeSitterProjectParser::new(2).unwrap(),
        );
        let scan = indexer.prepare(ScanScope::Full).unwrap();
        let mut projection = SemanticIndexProjection::new(&root, PROVIDER_INSTANCE_ID).unwrap();
        let batch = projection.project(&scan).unwrap();
        let pages = snapshot_pages(&batch, "job", true);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["replace_paths"], json!([]));
        assert_eq!(pages[0]["ingestion_page_count"], json!(1));
        assert_eq!(
            pages[0]["semantic_sources"]
                .as_array()
                .expect("source upsert")
                .len(),
            1
        );
        assert_eq!(pages[0]["ingestion_job_id"], json!("job"));
        assert_eq!(pages[0]["ingestion_page_index"], json!(0));
    }

    /// Publication needs the compiled `builtin.semantic` plugin, which an
    /// in-memory app has no install of; the route must fail closed with a
    /// message naming the missing tool instead of reporting success.
    #[test]
    fn publication_failure_fails_closed_when_semantic_plugin_is_absent() {
        let app = credentialed_app();
        let directory = fixture_tree();
        let response = project_import_response(
            &app,
            &request(json!({"folderPath": directory.path().to_string_lossy()})),
        );
        assert_eq!(response.status, "500 Internal Server Error");
        let body = response_body(&response);
        let message = body.to_string();
        assert!(
            message.contains("builtin.semantic/ingest_index_batch unavailable"),
            "unexpected failure message: {message}"
        );
        let mut sessions = app.project_import_sessions.lock().unwrap();
        assert_eq!(sessions.len(), 1);
        let retry = sessions
            .values_mut()
            .next()
            .unwrap()
            .indexer
            .prepare(ScanScope::Full)
            .unwrap();
        assert!(
            retry.is_full_snapshot(),
            "failed publication must remain retryable"
        );
    }

    #[test]
    fn retained_import_session_detects_edits_renames_and_removals() {
        let directory = fixture_tree();
        let mut session = import_session(directory.path()).ok().unwrap();
        let initial = session.indexer.prepare(ScanScope::Full).unwrap();
        session.indexer.commit(initial).unwrap();
        let unchanged = session.indexer.prepare(ScanScope::Full).unwrap();
        assert!(!unchanged.needs_publication());
        session.indexer.commit(unchanged).unwrap();
        fs::rename(
            directory.path().join("src/main.rs"),
            directory.path().join("src/renamed.rs"),
        )
        .unwrap();
        fs::write(directory.path().join("README.md"), "# Updated guide\n").unwrap();
        let changed = session.indexer.prepare(ScanScope::Full).unwrap();
        let batch = session.projection.project(&changed).unwrap();
        assert!(!changed.is_full_snapshot());
        assert!(batch.removed_paths.contains(&"src/main.rs".to_string()));
        assert!(
            batch
                .semantic_elements
                .iter()
                .any(|element| element.path == "src/renamed.rs")
        );
        assert!(
            batch
                .semantic_elements
                .iter()
                .any(|element| element.semantic_element_name == "Updated guide")
        );
    }

    #[test]
    fn pz_source_without_snapshot_reports_missing_archive() {
        let app = credentialed_app();
        let directory = fixture_tree();
        let response = project_import_response(
            &app,
            &request(json!({
                "folderPath": directory.path().to_string_lossy(),
                "source": "pz",
            })),
        );
        assert_eq!(response.status, "404 Not Found");
        let message = response_body(&response).to_string();
        assert!(
            message.contains("project has no linked snapshot at") && message.contains("graph.pz"),
            "unexpected failure message: {message}"
        );
    }

    #[test]
    fn unknown_import_source_is_rejected() {
        let app = credentialed_app();
        let directory = fixture_tree();
        let response = project_import_response(
            &app,
            &request(json!({
                "folderPath": directory.path().to_string_lossy(),
                "source": "zip",
            })),
        );
        assert_eq!(response.status, "400 Bad Request");
        let message = response_body(&response).to_string();
        assert!(
            message.contains("invalid import source `zip`") && message.contains("expected"),
            "unexpected failure message: {message}"
        );
    }

    /// End-to-end PZ import: folder import -> `CreatePzSnapshot` -> PZ route
    /// import -> the refresh poller must leave the restored elements in place.
    /// Needs the real `ImportPzSnapshot` implementation; the db-core stub
    /// ("PZ import pending implementation") fails this test.
    #[test]
    fn pz_import_publishes_snapshot_structure_and_fails_closed_without_index_log() {
        // The in-memory app has no compiled `builtin.semantic`, so the
        // provenance log cannot be written; the route must still publish the
        // archive through DB Core and then report the missing provenance.
        let app = credentialed_app();
        let control = InvocationControl::sixty_seconds();
        app.semantic
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: "/seed".into(),
                    elements: vec![lumvise_db_core::SemanticElement {
                        project_root: "/seed".into(),
                        semantic_element_id: "seed-file".into(),
                        semantic_source_id: "seed-source".into(),
                        path: "src/main.rs".into(),
                        element_kind: "file".into(),
                        name: "main.rs".into(),
                        parent_element_id: None,
                        content_fingerprint: None,
                        start_line: None,
                        end_line: None,
                        lifecycle: "active".into(),
                        match_evidence: None,
                        metadata: json!({}),
                    }],
                    relationships: vec![],
                },
                &control,
            )
            .unwrap();
        let linked = TempDir::new().unwrap();
        let archive = linked.path().join(".lv").join("graph.pz");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        app.semantic
            .execute(
                SemanticOperation::CreatePzSnapshot {
                    project_root: "/seed".into(),
                    output_path: archive.to_string_lossy().into_owned(),
                },
                &control,
            )
            .unwrap();
        let project_root = linked
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let response = project_import_response(
            &app,
            &request(json!({"folderPath": project_root, "source": "pz"})),
        );

        assert_eq!(response.status, "500 Internal Server Error");
        let message = response_body(&response).to_string();
        assert!(
            message.contains("completion log failed"),
            "unexpected failure message: {message}"
        );
        match app
            .semantic
            .execute(
                SemanticOperation::ProjectElementCounts { project_root },
                &control,
            )
            .unwrap()
        {
            SemanticResult::ProjectElementCounts { total_elements, .. } => {
                assert_eq!(total_elements, 1)
            }
            other => panic!("unexpected element counts result: {other:?}"),
        }
    }
}
