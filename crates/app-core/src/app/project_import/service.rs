//! One folder scanner/publication owner for native and delegated imports.
//! Call `import_folder` for controlled compiled callers; native adapters use `import_native`.
//! Session/checkpoint internals stay in this module and the existing publication owner.
use super::*;
use lumvise_db_core::{RelationalPersistence, SemanticPersistence};
use lumvise_frontend_core::FrontendCore;
use lumvise_plugin_runtime::{
    HostCapabilityError, HostPluginInvocation, HostPluginInvoker, PluginInvocationContext,
};
use std::sync::{Mutex, OnceLock};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FolderImportInput {
    folder_path: String,
    project_root: Option<String>,
    replace_paths: Option<Vec<String>>,
}

pub(crate) struct ProjectFolderImportService {
    pub(super) semantic: Arc<dyn SemanticPersistence>,
    pub(super) relational: Arc<dyn RelationalPersistence>,
    pub(super) index_publication_client: Arc<OnceLock<String>>,
    sessions: Arc<Mutex<ProjectImportSessions>>,
    frontend: Arc<Mutex<FrontendCore>>,
    converter: Arc<Mutex<Arc<lumvise_project_indexer::DocumentConverter>>>,
}

pub(super) struct ImportExecution<'a> {
    pub(super) control: &'a InvocationControl,
    nested: &'a dyn HostPluginInvoker,
    log_plugin_id: &'a str,
}

impl ProjectFolderImportService {
    pub(crate) fn new(
        semantic: Arc<dyn SemanticPersistence>,
        relational: Arc<dyn RelationalPersistence>,
        index_publication_client: Arc<OnceLock<String>>,
        sessions: Arc<Mutex<ProjectImportSessions>>,
        frontend: Arc<Mutex<FrontendCore>>,
        converter: Arc<Mutex<Arc<lumvise_project_indexer::DocumentConverter>>>,
    ) -> Self {
        Self {
            semantic,
            relational,
            index_publication_client,
            sessions,
            frontend,
            converter,
        }
    }

    /// Imports one authorized folder with the caller's lifecycle and nested Semantic authority.
    /// Example: `service.import_folder(json!({"folderPath": root}), context, nested)`.
    pub(crate) fn import_folder(
        &self,
        input: Value,
        context: &PluginInvocationContext,
        nested: &dyn HostPluginInvoker,
    ) -> Result<Value, HostCapabilityError> {
        let request: FolderImportInput = serde_json::from_value(input.clone()).map_err(|error| HostCapabilityError::new(
            "project.import", "invalid_import_input", format!("folder import {input}; expected {{folderPath:string,projectRoot?:string,replacePaths?:string[]}}: {error}"), false))?;
        let bridge = crate::plugin::PluginHostServices::project_import_control(context);
        let control = bridge.control();
        let execution = ImportExecution {
            control: &control,
            nested,
            log_plugin_id: "builtin.canvas",
        };
        import_project(self, request, false, &execution).map_err(import_host_error)
    }

    pub(super) fn import_native(
        &self,
        input: ImportInput,
        app: &AppCore,
        existing_only: bool,
    ) -> Result<Value, HttpResponse> {
        let publisher = super::publication::NativeImportPublisher::new(app);
        let control = publisher.control();
        let execution = ImportExecution {
            control: &control,
            nested: &publisher,
            log_plugin_id: IMPORTER_PLUGIN_ID,
        };
        let input = FolderImportInput {
            folder_path: input.folder_path,
            project_root: input.project_root,
            replace_paths: input.replace_paths,
        };
        import_project(self, input, existing_only, &execution)
    }

    fn converter(&self) -> Result<Arc<lumvise_project_indexer::DocumentConverter>, String> {
        super::super::document_conversion::shared_converter(&self.frontend, &self.converter)
    }

    fn project_is_indexed(
        &self,
        root: &str,
        control: &InvocationControl,
    ) -> Result<bool, HttpResponse> {
        match self
            .semantic
            .execute(SemanticOperation::ProjectRoots, control)
            .map_err(revisions::source_failure)?
        {
            SemanticResult::ProjectRoots(roots) => Ok(roots.iter().any(|entry| entry == root)),
            other => Err(revisions::source_failure(format!(
                "project roots {other:?}; expected ProjectRoots"
            ))),
        }
    }
}

fn import_host_error(response: HttpResponse) -> HostCapabilityError {
    HostCapabilityError::new(
        "project.import",
        "project_import_failed",
        String::from_utf8_lossy(response.buffered_bytes().unwrap_or_default()).into_owned(),
        false,
    )
}

fn active_import(control: &InvocationControl) -> Result<(), HttpResponse> {
    if control.is_cancelled() || control.is_expired() {
        return Err(revisions::source_failure(
            "folder import cancelled or deadline expired; expected active inherited invocation",
        ));
    }
    Ok(())
}

pub(super) fn semantic(
    execution: &ImportExecution<'_>,
    export_id: &str,
    input: Value,
) -> Result<Value, String> {
    active_import(execution.control).map_err(|response| {
        String::from_utf8_lossy(response.buffered_bytes().unwrap_or_default()).into_owned()
    })?;
    execution
        .nested
        .invoke_plugin(HostPluginInvocation {
            plugin_id: SEMANTIC_PLUGIN_ID.into(),
            export_id: export_id.into(),
            input,
        })
        .map_err(|error| error.to_string())
}

fn record_index_log(execution: &ImportExecution<'_>, entry: Value) -> Result<Value, String> {
    semantic(execution, RECORD_INDEX_LOG_EXPORT_ID, entry)
}

pub(super) fn import_session(
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
        source_checkout: None,
        refresh_retry: None,
    })
}

fn import_project(
    service: &ProjectFolderImportService,
    input: FolderImportInput,
    existing_only: bool,
    execution: &ImportExecution<'_>,
) -> Result<Value, HttpResponse> {
    active_import(execution.control)?;
    let started_at = Utc::now().to_rfc3339();
    let root = canonical_root(&input.folder_path)?;
    let project_root = input
        .project_root
        .unwrap_or_else(|| root.to_string_lossy().into_owned());

    // Keep scan acknowledgements across refreshes. Serialize publication with
    // manual imports so a delayed batch cannot overwrite a newer source state.
    let mut sessions = service.sessions.lock().map_err(|_| {
        error_response(
            "500 Internal Server Error",
            "project import sessions poisoned; expected available importer",
        )
    })?;
    // The refresh catalog is read before this lock. Recheck inside the same
    // lifecycle gate as removal so an old catalog cannot resurrect a project.
    if existing_only && !service.project_is_indexed(&project_root, execution.control)? {
        return Ok(finished(&project_root, 0, 0, 0, Vec::new(), &started_at));
    }
    let converter = service
        .converter()
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
        service,
        session,
        &root,
        &project_root,
        input.replace_paths,
        configuration_changed,
        &started_at,
        execution,
    );
    session.complete_refresh_attempt(&result, now, &project_root);
    result
}

/// Scans one session's changes and publishes them as staged snapshot pages.
fn publish_scan(
    service: &ProjectFolderImportService,
    session: &mut ProjectImportSession,
    root: &Path,
    project_root: &str,
    replace_paths: Option<Vec<String>>,
    configuration_changed: bool,
    started_at: &str,
    execution: &ImportExecution<'_>,
) -> Result<Value, HttpResponse> {
    revisions::prepare_checkout(service, session, root, project_root, execution.control)?;
    let ProjectImportSession {
        indexer,
        projection,
        source_checkout,
        ..
    } = session;
    let checkout = source_checkout
        .as_ref()
        .ok_or_else(|| revisions::source_failure("missing source checkout"))?;
    revisions::replay_pending(checkout, execution)?;
    let before = checkout
        .before_scan(execution.control)
        .map_err(revisions::source_failure)?;
    let scope = match &replace_paths {
        Some(paths)
            if !paths.is_empty() && !configuration_changed && !before.requires_full_scan =>
        {
            ScanScope::Paths(paths.clone())
        }
        _ => ScanScope::Full,
    };
    active_import(execution.control)?;
    let scan = indexer.prepare(scope).map_err(scan_failure)?;
    active_import(execution.control)?;
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
    let Some(frozen) = checkout
        .freeze_batch(batch, full_snapshot, before, execution.control)
        .map_err(revisions::source_failure)?
    else {
        indexer.commit(scan).map_err(scan_failure)?;
        return Ok(finished(
            project_root,
            files_scanned,
            0,
            0,
            skipped,
            started_at,
        ));
    };
    let batch = &frozen.batch;
    let elements_upserted = batch.semantic_elements.len();
    let relationships_upserted = batch.semantic_relationships.len();

    let job_id = &frozen.operation_id;
    let pages = snapshot_pages(
        batch,
        job_id,
        batch.replace_paths.is_empty() && batch.removed_paths.is_empty(),
    );
    let page_count = pages.len();
    let source_id = batch
        .semantic_sources
        .first()
        .map(|source| source.semantic_source_id.clone());

    // Best-effort start marker so the UI can observe an in-flight import.
    let _ = record_index_log(
        execution,
        json!({
            "provider_instance_id": PROVIDER_INSTANCE_ID,
            "plugin_id": execution.log_plugin_id,
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

    let outcome = publish_pages(execution, pages);
    if let Err(error) = &outcome {
        // Terminal state record; the staging area itself expires via TTL.
        let _ = record_index_log(
            execution,
            json!({
                "provider_instance_id": PROVIDER_INSTANCE_ID,
                "plugin_id": execution.log_plugin_id,
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
        )
        .inspect_err(|log_error| {
            tracing::warn!(event = "project_import_terminal_log_failed",
                project_root, index_log_id = %job_id, error = %log_error);
        });
        return Err(error_response(
            "500 Internal Server Error",
            format!("project import failed: {error}"),
        ));
    }
    let receipt = outcome.map_err(revisions::source_failure)?;
    checkout
        .acknowledge(&receipt, execution.control)
        .map_err(revisions::source_failure)?;
    indexer.commit(scan).map_err(scan_failure)?;

    record_index_log(
        execution,
        json!({
            "provider_instance_id": PROVIDER_INSTANCE_ID,
            "plugin_id": execution.log_plugin_id,
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

/// Splits one projection batch into staged ingestion pages. Page 0 carries the
/// source upsert and the complete replacement partition (`replace_paths` +
/// `removed_paths`) because the host takes the partition from the first page;
/// a full snapshot deliberately keeps `replace_paths` empty so the commit
/// replaces the whole project structure. Later pages carry the paths their
/// elements cover (partial imports only).
pub(super) fn snapshot_pages<'a>(
    batch: &'a IndexBatchRequest,
    job_id: &'a str,
    full_snapshot: bool,
) -> SemanticIndexPages<'a> {
    SemanticIndexPages::new(batch, job_id, full_snapshot)
}

/// Publishes every staged page through the semantic plugin's
/// `ingest_index_batch` MCP tool.
pub(super) fn publish_pages(
    execution: &ImportExecution<'_>,
    pages: SemanticIndexPages<'_>,
) -> Result<lumvise_db_core::SemanticRevisionPublicationReceipt, String> {
    let mut receipt = None;
    for page in pages {
        let input = serde_json::to_value(page)
            .map_err(|error| format!("cannot serialize frozen ingestion page: {error}"))?;
        let response = semantic(execution, INGEST_EXPORT_ID, input)?;
        if response["accepted"] != json!(true) {
            return Err(format!(
                "semantic ingestion rejected an import page: {response}"
            ));
        }
        if !response["publication"].is_null() {
            let outcome: lumvise_db_core::SemanticRevisionPublicationOutcome =
                serde::Deserialize::deserialize(&response["publication"]).map_err(|error| {
                    format!(
                        "invalid Semantic publication `{}`; expected typed outcome: {error}",
                        response["publication"]
                    )
                })?;
            match outcome {
                lumvise_db_core::SemanticRevisionPublicationOutcome::Accepted(accepted) => {
                    receipt = Some(accepted)
                }
                lumvise_db_core::SemanticRevisionPublicationOutcome::Conflict(conflict) => {
                    return Err(format!("source publication conflict: {conflict:?}"));
                }
            }
        }
    }
    receipt.ok_or_else(|| "Semantic completed pages without an accepted publication receipt".into())
}

/// Validates and canonicalizes the requested folder.
pub(super) fn canonical_root(folder_path: &str) -> Result<PathBuf, HttpResponse> {
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
pub(super) fn accepted_paths(scan: &PreparedProjectScan) -> BTreeSet<String> {
    scan.changed_files()
        .map(|file| file.entry.path.clone())
        .collect()
}

/// Walks the folder once and reports the exclusion points the scanner applied:
/// every path that is absent from the accepted inventory, without descending
/// into excluded subtrees (one entry per exclusion gate, not per file).
pub(super) fn collect_excluded(root: &Path, accepted: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut skipped = Vec::new();
    walk_excluded(root, "", accepted, &mut skipped);
    skipped
}

pub(super) fn walk_excluded(
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
pub(super) fn parser_workers() -> usize {
    std::thread::available_parallelism()
        .map(|workers| workers.get().min(4))
        .unwrap_or(2)
}

pub(super) fn finished(
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

pub(super) fn scan_failure(error: ScanError) -> HttpResponse {
    error_response("500 Internal Server Error", error.to_string())
}
