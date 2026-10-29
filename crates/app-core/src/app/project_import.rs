//! App-owned project import for the Knowledge Canvas UI.
//!
//! UI/background and authorized compiled callers share `ProjectFolderImportService`.
//! The service reads source in the app process; compiled callers receive no
//! filesystem handles or broader sandbox access. Semantic still owns ingestion.
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
    ScanError, ScanScope, SemanticIndexPages, SemanticIndexProjection, SourceKind,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::{AppCore, PluginInvocationRequest, PluginInvocationStatus};
mod publication;
mod revisions;
mod service;
pub(crate) use service::ProjectFolderImportService;
#[cfg(test)]
use service::{accepted_paths, collect_excluded, import_session, snapshot_pages};
use service::{canonical_root, finished, scan_failure};

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
    source_checkout: Option<crate::index_publication::IndexSourceCheckout>,
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

impl ProjectImportSession {
    fn complete_refresh_attempt(
        &mut self,
        result: &Result<Value, HttpResponse>,
        now: Instant,
        root: &str,
    ) {
        self.refresh_retry = match result {
            Ok(_) => None,
            Err(_) => {
                let retry = RefreshRetry::after_failure(self.refresh_retry, now);
                tracing::warn!(
                    event = "project_refresh_deferred",
                    project_root = root,
                    failures = retry.failures,
                    retry_in_secs = (retry.not_before - now).as_secs()
                );
                Some(retry)
            }
        };
    }
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
    app.plugin_host_services
        .project_import_service()
        .import_native(input, app, existing_only)
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

#[cfg(test)]
mod shared_service_tests;
#[cfg(test)]
mod tests;
