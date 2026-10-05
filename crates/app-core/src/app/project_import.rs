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
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    document_converter: Arc<lumvise_project_indexer::DocumentConverter>,
    refresh_retry: Option<RefreshRetry>,
}

/// First wait before a failed project is refreshed again; doubles per
/// consecutive failure up to [`REFRESH_RETRY_MAX`].
const REFRESH_RETRY_BASE: Duration = Duration::from_secs(60);
const REFRESH_RETRY_MAX: Duration = Duration::from_secs(3600);

/// Background-refresh deferral after a failed import. A deterministic failure,
/// such as a commit that cannot finish within the plugin invocation deadline,
/// would otherwise repeat every refresh round, restarting the semantic plugin
/// and growing the graph WAL each time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RefreshRetry {
    failures: u32,
    not_before: Instant,
}

impl RefreshRetry {
    /// Waits 1, 2, 4 ... minutes after consecutive failures, at most one hour.
    fn after_failure(previous: Option<Self>, now: Instant) -> Self {
        let failures = previous.map_or(1, |retry| retry.failures.saturating_add(1));
        let delay = REFRESH_RETRY_BASE
            .saturating_mul(1 << (failures - 1).min(6))
            .min(REFRESH_RETRY_MAX);
        Self {
            failures,
            not_before: now + delay,
        }
    }
}

fn import_session(
    root: &Path,
    converter: Arc<lumvise_project_indexer::DocumentConverter>,
) -> Result<ProjectImportSession, HttpResponse> {
    Ok(ProjectImportSession {
        document_converter: Arc::clone(&converter),
        indexer: ProjectIndexer::new(
            FilesystemProjectSource::open(root).map_err(scan_failure)?,
            ParallelTreeSitterProjectParser::new_with_document_converter(
                parser_workers(),
                Default::default(),
                converter,
            )
            .map_err(scan_failure)?,
        ),
        projection: SemanticIndexProjection::new(root, PROVIDER_INSTANCE_ID)
            .map_err(scan_failure)?,
        refresh_retry: None,
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
        if let Err(response) = import_project(
            app,
            ImportInput {
                folder_path: root.into(),
                project_root: Some(root.into()),
                replace_paths: None,
                source: None,
            },
            true,
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
    import_project(app, input, false)
}

fn import_project(
    app: &AppCore,
    input: ImportInput,
    existing_only: bool,
) -> Result<Value, HttpResponse> {
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
    // The refresh catalog is read before this lock. Recheck inside the same
    // lifecycle gate as removal so an old catalog cannot resurrect a project.
    if existing_only && !project_is_indexed(app, &project_root)? {
        return Ok(finished(&project_root, 0, 0, 0, Vec::new(), &started_at));
    }
    let converter = super::document_conversion::converter(app)
        .map_err(|error| error_response("500 Internal Server Error", error))?;
    let mut configuration_changed = false;
    let session = match sessions.entry((root.clone(), project_root.clone())) {
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if !Arc::ptr_eq(&entry.get().document_converter, &converter) {
                entry.insert(import_session(&root, converter)?);
                configuration_changed = true;
            }
            entry.into_mut()
        }
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(import_session(&root, converter)?)
        }
    };
    let now = Instant::now();
    if existing_only
        && session
            .refresh_retry
            .is_some_and(|retry| now < retry.not_before)
    {
        // A failed refresh waits out its backoff; manual imports still run.
        return Ok(finished(&project_root, 0, 0, 0, Vec::new(), &started_at));
    }
    let result = publish_scan(
        app,
        session,
        &root,
        &project_root,
        input.replace_paths,
        configuration_changed,
        &started_at,
    );
    session.refresh_retry = match &result {
        Ok(_) => None,
        Err(_) => {
            let retry = RefreshRetry::after_failure(session.refresh_retry, now);
            tracing::warn!(event = "project_refresh_deferred", project_root = %project_root,
                failures = retry.failures, retry_in_secs = (retry.not_before - now).as_secs());
            Some(retry)
        }
    };
    result
}

/// Scans one session's changes and publishes them as staged snapshot pages.
fn publish_scan(
    app: &AppCore,
    session: &mut ProjectImportSession,
    root: &Path,
    project_root: &str,
    replace_paths: Option<Vec<String>>,
    configuration_changed: bool,
    started_at: &str,
) -> Result<Value, HttpResponse> {
    let ProjectImportSession {
        indexer,
        projection,
        ..
    } = session;
    let scope = match &replace_paths {
        Some(paths) if !paths.is_empty() && !configuration_changed => {
            ScanScope::Paths(paths.clone())
        }
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
        return Ok(finished(project_root, 0, 0, 0, Vec::new(), started_at));
    }

    let mut batch = projection.project(&scan).map_err(scan_failure)?;
    let skipped = if full_snapshot {
        collect_excluded(root, &accepted_paths(&scan))
    } else {
        // A selective import replaces exactly the requested paths; policy
        // exclusions outside that scope are not part of this import.
        Vec::new()
    };
    // The caller-supplied identity wins so scoped reads later match the
    // returned `projectRoot`; the source upsert keeps the real folder path.
    batch.project_root = project_root.to_owned();
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
        project_root,
        files_scanned,
        elements_upserted,
        relationships_upserted,
        skipped,
        started_at,
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
    // Serialize the shared fields once; re-serializing the whole batch per
    // page made paging quadratic in project size.
    let mut header = serde_json::to_value(batch).expect("index batch serializes");
    let fields = header.as_object_mut().expect("index batch is an object");
    for paged in [
        "semantic_elements",
        "semantic_relationships",
        "semantic_sources",
        "removed_paths",
        "replace_paths",
    ] {
        fields.remove(paged);
    }
    (0..page_count)
        .map(|index| {
            let element_window = window(index, ELEMENTS_PER_PAGE, elements);
            let relationship_window = window(index, RELATIONSHIPS_PER_PAGE, relationships);
            let mut page = header.clone();
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

fn project_is_indexed(app: &AppCore, project_root: &str) -> Result<bool, HttpResponse> {
    let result = app
        .semantic
        .execute(
            SemanticOperation::ProjectRoots,
            &InvocationControl::sixty_seconds(),
        )
        .map_err(|error| error_response("500 Internal Server Error", error.to_string()))?;
    match result {
        SemanticResult::ProjectRoots(roots) => Ok(roots.iter().any(|root| root == project_root)),
        other => Err(error_response(
            "500 Internal Server Error",
            format!("invalid project catalog `{other:?}`; expected ProjectRoots"),
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
mod tests;
