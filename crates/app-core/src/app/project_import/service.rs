//! One folder scanner/publication owner for native and delegated imports.
//! Semantic calls `import_folder`; native HTTP uses `import_native_request`.
//! Automatic refresh drives that same lifecycle through `import_native`.
//! Session/checkpoint internals stay in this module and the existing publication owner.
use super::*;
use lumvise_db_core::{RelationalPersistence, SemanticPersistence};
use lumvise_frontend_core::FrontendCore;
use lumvise_plugin_runtime::{
    HostCapabilityError, HostPluginInvocation, HostPluginInvoker, PluginInvocationContext,
};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FolderImportInput {
    pub(super) folder_path: String,
    pub(super) project_root: Option<String>,
    pub(super) replace_paths: Option<Vec<String>>,
    pub(super) import_id: Option<String>,
    #[serde(default)]
    pub(super) cancel: bool,
    #[serde(default)]
    pub(super) status_only: bool,
    #[serde(skip)]
    pub(super) existing_only: bool,
}

pub(crate) struct ProjectFolderImportService {
    pub(super) semantic: Arc<dyn SemanticPersistence>,
    pub(super) relational: Arc<dyn RelationalPersistence>,
    pub(super) index_publication_client: Arc<OnceLock<String>>,
    pub(super) sessions: Arc<Mutex<ProjectImportSessions>>,
    frontend: Arc<Mutex<FrontendCore>>,
    converter: Arc<Mutex<Arc<lumvise_project_indexer::DocumentConverter>>>,
    pub(super) jobs: super::jobs::ImportJobs,
    runtime: OnceLock<std::sync::Weak<lumvise_plugin_runtime::PluginSystem>>,
}

pub(super) struct ImportExecution<'a> {
    pub(super) control: &'a InvocationControl,
    pub(super) nested: &'a dyn HostPluginInvoker,
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
            jobs: Default::default(),
            runtime: OnceLock::new(),
        }
    }

    pub(crate) fn install_runtime(&self, runtime: &Arc<lumvise_plugin_runtime::PluginSystem>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
    }

    pub(crate) fn publisher(
        &self,
        scope: Option<PluginInvocationContext>,
        authorization: Option<Arc<dyn ImportAuthorization>>,
    ) -> Arc<dyn ImportPublicationPort> {
        Arc::new(ProjectImportPublisher::new(
            self.runtime.get().cloned().unwrap_or_default(),
            scope,
            authorization,
        ))
    }

    /// Starts or observes one authorized job; CPU and publication never borrow the caller's process channel.
    /// Example: call with `{ "folderPath": root }`, then repeat with the returned `importId`.
    pub(crate) fn import_folder(
        self: &Arc<Self>,
        input: Value,
        context: &PluginInvocationContext,
        publisher: Arc<dyn ImportPublicationPort>,
    ) -> Result<Value, HostCapabilityError> {
        let request = decode_request(input).map_err(import_host_error)?;
        let bridge = crate::plugin::PluginHostServices::project_import_control(context);
        active_import(&bridge.control()).map_err(import_host_error)?;
        self.schedule(
            request,
            context.session_id().unwrap_or("unbound"),
            publisher,
            &bridge.control(),
        )
        .map_err(import_host_error)
    }

    pub(crate) fn import_native_request(
        self: &Arc<Self>,
        mut input: Value,
        _app: &AppCore,
    ) -> Result<Value, HttpResponse> {
        if let Some(object) = input.as_object_mut() {
            object.remove("source");
        }
        self.schedule(
            decode_request(input)?,
            "native-human",
            self.publisher(None, None),
            &InvocationControl::sixty_seconds(),
        )
    }

    fn schedule(
        self: &Arc<Self>,
        mut input: FolderImportInput,
        owner: &str,
        publisher: Arc<dyn ImportPublicationPort>,
        control: &InvocationControl,
    ) -> Result<Value, HttpResponse> {
        let observe_only = input.status_only;
        input.status_only = true;
        let execution = ImportExecution {
            control,
            nested: publisher.as_ref(),
        };
        let status = super::jobs::import_job_request(self, input.clone(), owner, &execution)?;
        if observe_only
            || (!input.cancel
                && matches!(
                    status["phase"].as_str(),
                    Some("failed" | "canceled" | "completed" | "completed_with_errors")
                ))
        {
            return Ok(status);
        }
        input.status_only = false;
        input.import_id = status["importId"].as_str().map(str::to_owned);
        super::jobs::schedule_import_publication(self.clone(), input, owner.into(), publisher)?;
        Ok(status)
    }

    pub(crate) fn with_project_imports_stopped<T>(
        &self,
        root: &str,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.jobs.with_project_stopped(root, action)
    }

    pub(super) fn import_native(
        self: &Arc<Self>,
        input: ImportInput,
        _app: &AppCore,
        existing_only: bool,
    ) -> Result<Value, HttpResponse> {
        let root = canonical_root(&input.folder_path)?;
        let route = input
            .project_root
            .clone()
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        if existing_only
            && (self.jobs.active(&route)
                || !self.project_is_indexed(&route, &InvocationControl::sixty_seconds())?
                || self.refresh_deferred(&root, &route)?)
        {
            return Ok(finished(&route, 0, 0, 0, vec![], &Utc::now().to_rfc3339()));
        }
        let mut request = json!({"folderPath":input.folder_path,"projectRoot":input.project_root,"replacePaths":input.replace_paths});
        loop {
            let mut input = decode_request(request.clone())?;
            input.existing_only = existing_only;
            let status = self.schedule(
                input,
                "native-human",
                self.publisher(None, None),
                &InvocationControl::sixty_seconds(),
            )?;
            match status["phase"].as_str() {
                Some("completed" | "completed_with_errors") => return Ok(status["result"].clone()),
                Some("failed" | "canceled") => {
                    return Err(error_response(
                        "500 Internal Server Error",
                        status["error"].to_string(),
                    ));
                }
                _ => {
                    request["importId"] = status["importId"].clone();
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    fn refresh_deferred(&self, root: &Path, route: &str) -> Result<bool, HttpResponse> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| error_response("500 Internal Server Error", "import sessions poisoned"))?;
        Ok(sessions
            .get(&(root.to_path_buf(), route.into()))
            .is_some_and(|session| {
                session
                    .refresh_retry
                    .is_some_and(|retry| Instant::now() < retry.not_before)
            }))
    }

    pub(super) fn converter(
        &self,
    ) -> Result<Arc<lumvise_project_indexer::DocumentConverter>, String> {
        super::super::document_conversion::shared_converter(&self.frontend, &self.converter)
    }

    pub(super) fn project_is_indexed(
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

pub(super) fn active_import(control: &InvocationControl) -> Result<(), HttpResponse> {
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

pub(super) fn record_index_log(
    execution: &ImportExecution<'_>,
    entry: Value,
) -> Result<Value, String> {
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

#[cfg(test)]
pub(super) fn snapshot_pages<'a>(
    batch: &'a IndexBatchRequest,
    job_id: &'a str,
    full_snapshot: bool,
) -> SemanticIndexPages<'a> {
    SemanticIndexPages::new(batch, job_id, full_snapshot)
}

fn decode_request(input: Value) -> Result<FolderImportInput, HttpResponse> {
    serde_json::from_value(input.clone()).map_err(|error| error_response("400 Bad Request", format!(
        "folder import {input}; expected {{folderPath:string,projectRoot?:string,replacePaths?:string[],importId?:string,cancel?:boolean,statusOnly?:boolean}}: {error}")))
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
        .chain(scan.failures().iter().map(|error| error.path.clone()))
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
