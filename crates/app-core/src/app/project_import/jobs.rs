//! Continuing imports keep CPU preparation outside plugin lifetimes. Each request
//! reauthorizes at the existing host boundary before advancing Semantic staging.
use super::service::{FolderImportInput, ImportExecution, record_index_log};
use super::*;
use crate::index_publication::{FrozenIndexPublication, IndexSourceBase};
use lumvise_project_indexer::ScanMetrics;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// Finished jobs kept per project. A refresh can start before a caller has read the
/// previous completion, so the newest handles stay pollable; older ones are dropped.
pub(super) const RETAINED_FINISHED_JOBS: usize = 3;

#[derive(Default)]
pub(super) struct ImportJobs {
    entries: Mutex<BTreeMap<String, Arc<ImportJob>>>,
}

struct ImportJob {
    id: String,
    root: PathBuf,
    route: String,
    owner: String,
    input: FolderImportInput,
    created: Instant,
    started_at: String,
    cancel: Arc<AtomicBool>,
    advancing: AtomicBool,
    progress: Arc<Mutex<ScanMetrics>>,
    state: Mutex<ImportState>,
}

#[derive(Default)]
struct ImportState {
    receiver: Option<mpsc::Receiver<Result<PreparedImport, FailedPreparation>>>,
    prepared: Option<PreparedImport>,
    frozen: Option<FrozenIndexPublication>,
    next_page: usize,
    result: Option<Value>,
    error: Option<String>,
    canceled: bool,
    log_started: bool,
}

struct PreparedImport {
    session: ProjectImportSession,
    scan: Option<PreparedProjectScan>,
    batch: Option<IndexBatchRequest>,
    before: Option<IndexSourceBase>,
    failures: Vec<ScanError>,
    skipped: Vec<(String, String)>,
    files_scanned: usize,
    replay: Option<FrozenIndexPublication>,
}

struct FailedPreparation {
    session: ProjectImportSession,
    reason: String,
}

impl ImportJobs {
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.len())
    }

    pub(super) fn active(&self, route: &str) -> bool {
        self.entries
            .lock()
            .map(|entries| {
                entries.values().any(|job| {
                    job.route == route
                        && job.state.try_lock().map_or(true, |state| !state.terminal())
                })
            })
            .unwrap_or(true)
    }

    pub(super) fn with_project_stopped<T>(
        &self,
        route: &str,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "import registry poisoned")?;
        for job in entries.values().filter(|job| job.route == route) {
            job.cancel.store(true, Ordering::Release);
            job.state
                .lock()
                .map_err(|_| "import state poisoned")?
                .canceled = true;
        }
        // Keep starts fenced until graph removal/replacement completes.
        action()
    }
}

impl ImportState {
    fn terminal(&self) -> bool {
        self.canceled || self.error.is_some() || self.result.is_some()
    }

    fn phase(&self) -> &'static str {
        if self.canceled {
            return "canceled";
        }
        if self.error.is_some() {
            return "failed";
        }
        if let Some(result) = &self.result {
            return if result["failures"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            {
                "completed_with_errors"
            } else {
                "completed"
            };
        }
        if self.receiver.is_some() {
            "preparing"
        } else {
            "publishing"
        }
    }
}

pub(super) fn import_job_request(
    service: &Arc<ProjectFolderImportService>,
    input: FolderImportInput,
    owner: &str,
    execution: &ImportExecution<'_>,
) -> Result<Value, HttpResponse> {
    service::active_import(execution.control)?;
    let root = service::canonical_root(&input.folder_path)?;
    let route = input
        .project_root
        .clone()
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let job = match &input.import_id {
        Some(id) => authorized_import_job(service, id, &root, &route, owner)?,
        None => start_import_job(service, input.clone(), root, route, owner)?,
    };
    if job.input.replace_paths != input.replace_paths {
        return Err(error_response(
            "409 Conflict",
            "import replacePaths changed; expected the original request scope",
        ));
    }
    let Ok(mut state) = job.state.try_lock() else {
        return Ok(
            json!({"importId":job.id,"projectRoot":job.route,"phase":"publishing","canCancel":false,"filesScanned":0,"filesParsed":0,"entriesInspected":0,"pagesPublished":0,"result":null,"error":null}),
        );
    };
    if input.cancel {
        cancel_import_preparation(&job, &mut state)?;
    }
    if input.import_id.is_some() && !input.status_only && !state.terminal() {
        if let Err(error) = advance_import_publication(service, &job, &mut state, execution) {
            state.error = Some(import_failure_message(&error));
            if let Some(mut prepared) = state.prepared.take() {
                prepared.session.complete_refresh_attempt(
                    &Err(error_response(
                        "500 Internal Server Error",
                        state.error.clone().unwrap_or_default(),
                    )),
                    Instant::now(),
                    &job.route,
                );
                service
                    .sessions
                    .lock()
                    .map_err(|_| import_state_unavailable("import sessions"))?
                    .insert((job.root.clone(), job.route.clone()), prepared.session);
            }
            state.frozen = None;
            let _ = record_index_log(
                execution,
                json!({"provider_instance_id":PROVIDER_INSTANCE_ID,
                "plugin_id":"builtin.semantic","project_root":job.route,"index_log_id":job.id,
                "status":"failed","started_at":job.started_at,"completed_at":Utc::now().to_rfc3339(),
                "error":state.error,"metrics":{"phase":"failed"}}),
            );
        }
    }
    if state.canceled && !input.status_only {
        let _ = record_index_log(
            execution,
            json!({"provider_instance_id":PROVIDER_INSTANCE_ID,
            "plugin_id":"builtin.semantic","project_root":job.route,"index_log_id":job.id,
            "status":"canceled","started_at":job.started_at,"completed_at":Utc::now().to_rfc3339(),
            "metrics":{"phase":"canceled"}}),
        );
    }
    Ok(import_job_status(&job, &state))
}

fn authorized_import_job(
    service: &ProjectFolderImportService,
    id: &str,
    root: &Path,
    route: &str,
    owner: &str,
) -> Result<Arc<ImportJob>, HttpResponse> {
    service
        .jobs
        .entries
        .lock()
        .map_err(|_| import_state_unavailable("import registry"))?
        .get(id)
        .filter(|job| job.root == root && job.route == route && job.owner == owner)
        .cloned()
        .ok_or_else(|| {
            error_response(
                "404 Not Found",
                format!("import `{id}`; expected an import owned by this caller and project"),
            )
        })
}

fn start_import_job(
    service: &Arc<ProjectFolderImportService>,
    input: FolderImportInput,
    root: PathBuf,
    route: String,
    owner: &str,
) -> Result<Arc<ImportJob>, HttpResponse> {
    let mut entries = service
        .jobs
        .entries
        .lock()
        .map_err(|_| import_state_unavailable("import registry"))?;
    for job in entries.values().filter(|job| job.route == route) {
        if job.state.try_lock().map_or(true, |state| !state.terminal()) {
            return if job.owner == owner
                && job.root == root
                && job.input.replace_paths == input.replace_paths
            {
                Ok(job.clone())
            } else {
                Err(error_response(
                    "409 Conflict",
                    format!("project `{route}` already has an active import"),
                ))
            };
        }
    }
    // Every job of this route is finished here. Drop all but the newest handles so idle
    // refresh rounds cannot accumulate; the new job takes the last retained slot.
    let mut finished: Vec<_> = entries
        .values()
        .filter(|job| job.route == route)
        .map(|job| (job.created, job.id.clone()))
        .collect();
    finished.sort_unstable_by(|left, right| right.cmp(left));
    for (_, id) in finished.into_iter().skip(RETAINED_FINISHED_JOBS - 1) {
        entries.remove(&id);
    }
    let converter = service
        .converter()
        .map_err(|error| error_response("500 Internal Server Error", error))?;
    let mut session = service
        .sessions
        .lock()
        .map_err(|_| import_state_unavailable("import sessions"))?
        .remove(&(root.clone(), route.clone()))
        .map(Ok)
        .unwrap_or_else(|| service::import_session(&root, converter.clone()))?;
    if !Arc::ptr_eq(&session.document_converter, &converter) {
        session = service::import_session(&root, converter)?;
    }
    let job = Arc::new(ImportJob {
        id: format!("import-{}", uuid::Uuid::new_v4()),
        root,
        route,
        owner: owner.into(),
        input,
        created: Instant::now(),
        started_at: Utc::now().to_rfc3339(),
        cancel: Arc::default(),
        advancing: AtomicBool::default(),
        progress: Arc::default(),
        state: Mutex::default(),
    });
    {
        let mut state = job
            .state
            .lock()
            .map_err(|_| import_state_unavailable("import state"))?;
        spawn_import_preparation(service, &job, &mut state, session)?;
    }
    entries.insert(job.id.clone(), job.clone());
    Ok(job)
}

fn spawn_import_preparation(
    service: &Arc<ProjectFolderImportService>,
    job: &ImportJob,
    state: &mut ImportState,
    session: ProjectImportSession,
) -> Result<(), HttpResponse> {
    let (sender, receiver) = mpsc::channel();
    let service = service.clone();
    let input = job.input.clone();
    let cancel = job.cancel.clone();
    let progress = job.progress.clone();
    let route = job.route.clone();
    let root = job.root.clone();
    std::thread::Builder::new()
        .name("project-import-prepare".into())
        .spawn(move || {
            let _ = sender.send(prepare_import_session(
                &service, session, &input, &route, &root, &cancel, &progress,
            ));
        })
        .map_err(|error| {
            error_response(
                "500 Internal Server Error",
                format!("cannot start import preparation: {error}"),
            )
        })?;
    state.receiver = Some(receiver);
    state.frozen = None;
    state.next_page = 0;
    Ok(())
}

enum ImportSourcePreparation {
    Replay(FrozenIndexPublication),
    Scan(IndexSourceBase, ScanScope),
}

fn observe_import_source(
    service: &ProjectFolderImportService,
    session: &mut ProjectImportSession,
    input: &FolderImportInput,
    route: &str,
    root: &Path,
) -> Result<ImportSourcePreparation, HttpResponse> {
    let control = InvocationControl::sixty_seconds();
    if input.existing_only && !service.project_is_indexed(route, &control)? {
        return Err(error_response(
            "404 Not Found",
            format!("project `{route}` was removed before its refresh began"),
        ));
    }
    revisions::prepare_checkout(service, session, root, route, &control)?;
    let source = session
        .source_checkout
        .as_ref()
        .ok_or_else(|| import_state_unavailable("source checkout"))?;
    if let Some(frozen) = source
        .pending(&control)
        .map_err(revisions::source_failure)?
    {
        return Ok(ImportSourcePreparation::Replay(frozen));
    }
    let before = source
        .before_scan(&control)
        .map_err(revisions::source_failure)?;
    let scope = import_scan_scope(session, input, &before);
    Ok(ImportSourcePreparation::Scan(before, scope))
}

fn import_scan_scope(
    session: &ProjectImportSession,
    input: &FolderImportInput,
    before: &IndexSourceBase,
) -> ScanScope {
    match &input.replace_paths {
        Some(paths)
            if !paths.is_empty()
                && !before.requires_full_scan
                && session.indexer.has_inventory_baseline() =>
        {
            ScanScope::Paths(paths.clone())
        }
        _ => ScanScope::Full,
    }
}

fn prepare_import_session(
    service: &ProjectFolderImportService,
    mut session: ProjectImportSession,
    input: &FolderImportInput,
    route: &str,
    root: &Path,
    cancel: &AtomicBool,
    progress: &Mutex<ScanMetrics>,
) -> Result<PreparedImport, FailedPreparation> {
    if cancel.load(Ordering::Acquire) {
        return Err(FailedPreparation {
            session,
            reason: "import canceled before source observation".into(),
        });
    }
    match observe_import_source(service, &mut session, input, route, root) {
        Ok(ImportSourcePreparation::Replay(frozen)) => Ok(PreparedImport {
            session,
            scan: None,
            batch: None,
            before: None,
            failures: vec![],
            skipped: vec![],
            files_scanned: 0,
            replay: Some(frozen),
        }),
        Ok(ImportSourcePreparation::Scan(before, scope)) => {
            prepare_import_projection(session, before, scope, route, root, cancel, progress)
        }
        Err(error) => Err(FailedPreparation {
            session,
            reason: import_failure_message(&error),
        }),
    }
}

fn prepare_import_projection(
    mut session: ProjectImportSession,
    before: IndexSourceBase,
    scope: ScanScope,
    route: &str,
    root: &Path,
    cancel: &AtomicBool,
    progress: &Mutex<ScanMetrics>,
) -> Result<PreparedImport, FailedPreparation> {
    let outcome = scan_import_source(
        &mut session,
        scope,
        &before.invalidated_paths,
        route,
        root,
        cancel,
        progress,
    );
    match outcome {
        Ok(scan) => Ok(PreparedImport {
            session,
            failures: scan.scan.failures().to_vec(),
            files_scanned: scan.scan.metrics().files_read,
            scan: Some(scan.scan),
            batch: scan.batch,
            before: Some(before),
            skipped: scan.skipped,
            replay: None,
        }),
        Err(reason) => Err(FailedPreparation { session, reason }),
    }
}

struct ScanPreparation {
    scan: PreparedProjectScan,
    batch: Option<IndexBatchRequest>,
    skipped: Vec<(String, String)>,
}

fn scan_import_source(
    session: &mut ProjectImportSession,
    scope: ScanScope,
    invalidated_paths: &std::collections::BTreeSet<String>,
    route: &str,
    root: &Path,
    cancel: &AtomicBool,
    progress: &Mutex<ScanMetrics>,
) -> Result<ScanPreparation, String> {
    let scan = session
        .indexer
        .prepare_resilient_invalidating(scope, invalidated_paths, |metrics| {
            if cancel.load(Ordering::Acquire) {
                return Err(ScanError {
                    path: route.into(),
                    reason: "import canceled before publication".into(),
                });
            }
            if let Ok(mut current) = progress.lock() {
                *current = metrics.clone();
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let skipped = if scan.is_full_snapshot() {
        service::collect_excluded(root, &service::accepted_paths(&scan))
    } else {
        vec![]
    };
    let mut batch = scan
        .needs_publication()
        .then(|| session.projection.project(&scan))
        .transpose()
        .map_err(|error| error.to_string())?;
    if let Some(batch) = &mut batch {
        batch.project_root = route.into();
    }
    Ok(ScanPreparation {
        scan,
        batch,
        skipped,
    })
}

fn advance_import_publication(
    service: &Arc<ProjectFolderImportService>,
    job: &ImportJob,
    state: &mut ImportState,
    execution: &ImportExecution<'_>,
) -> Result<(), HttpResponse> {
    if let Some(receiver) = &state.receiver {
        match receiver.try_recv() {
            Ok(Ok(mut prepared)) => {
                state.frozen = prepared.replay.take();
                state.prepared = Some(prepared);
                state.receiver = None;
            }
            Ok(Err(failed)) => {
                state.prepared = Some(PreparedImport {
                    session: failed.session,
                    scan: None,
                    batch: None,
                    before: None,
                    failures: vec![],
                    skipped: vec![],
                    files_scanned: 0,
                    replay: None,
                });
                state.receiver = None;
                return Err(error_response("500 Internal Server Error", failed.reason));
            }
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(import_state_unavailable("preparation worker"));
            }
        }
    }
    if state.frozen.is_none() {
        freeze_import_projection(state, execution)?;
        if state.frozen.is_none() {
            return acknowledge_import_job(service, job, state, execution);
        }
        // Freezing can span many bounded storage calls; ingestion starts on a fresh step.
        return Ok(());
    }
    log_publication_started(job, state, execution);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if publish_import_page(state, execution)? {
            let control = InvocationControl::sixty_seconds();
            let completion = ImportExecution {
                control: &control,
                nested: execution.nested,
            };
            return acknowledge_import_job(service, job, state, &completion);
        }
    }
    Ok(())
}

/// Idle scans publish nothing and stay out of the index log; the start is logged once there is
/// a publication to follow, and `acknowledge_import_job` closes exactly the jobs that started.
fn log_publication_started(
    job: &ImportJob,
    state: &mut ImportState,
    execution: &ImportExecution<'_>,
) {
    if state.log_started {
        return;
    }
    let _ = record_index_log(
        execution,
        json!({"provider_instance_id":PROVIDER_INSTANCE_ID,
        "plugin_id":"builtin.semantic","project_root":job.route,"index_log_id":job.id,
        "status":"running","started_at":job.started_at,"metrics":{"phase":state.phase()}}),
    );
    state.log_started = true;
}

fn freeze_import_projection(
    state: &mut ImportState,
    execution: &ImportExecution<'_>,
) -> Result<(), HttpResponse> {
    let prepared = state
        .prepared
        .as_mut()
        .ok_or_else(|| import_state_unavailable("prepared import"))?;
    let Some(batch) = prepared.batch.take() else {
        return Ok(());
    };
    let failed: Vec<_> = prepared
        .failures
        .iter()
        .map(|error| error.path.clone())
        .collect();
    let full = prepared
        .scan
        .as_ref()
        .is_some_and(PreparedProjectScan::is_full_snapshot);
    state.frozen = prepared
        .session
        .source_checkout
        .as_ref()
        .ok_or_else(|| import_state_unavailable("source checkout"))?
        .freeze_batch_with_failures(
            batch,
            full,
            prepared
                .before
                .take()
                .ok_or_else(|| import_state_unavailable("source observation"))?,
            &failed,
            execution.control,
        )
        .map_err(revisions::source_failure)?;
    Ok(())
}

fn publish_import_page(
    state: &mut ImportState,
    execution: &ImportExecution<'_>,
) -> Result<bool, HttpResponse> {
    let frozen = state
        .frozen
        .as_ref()
        .ok_or_else(|| import_state_unavailable("frozen import"))?;
    let full = frozen.batch.replace_paths.is_empty() && frozen.batch.removed_paths.is_empty();
    let page = SemanticIndexPages::new(&frozen.batch, &frozen.operation_id, full)
        .nth(state.next_page)
        .ok_or_else(|| import_state_unavailable("next ingestion page"))?;
    let input = serde_json::to_value(page).map_err(revisions::source_failure)?;
    let response =
        service::semantic(execution, INGEST_EXPORT_ID, input).map_err(revisions::source_failure)?;
    if response["accepted"] != json!(true) {
        return Err(revisions::source_failure(format!(
            "Semantic rejected import page: {response}"
        )));
    }
    if response["publication"].is_null() {
        state.next_page += 1;
        return Ok(false);
    }
    let outcome: lumvise_db_core::SemanticRevisionPublicationOutcome =
        serde_json::from_value(response["publication"].clone()).map_err(|error| {
            revisions::source_failure(format!(
                "invalid Semantic publication `{}`; expected typed outcome: {error}",
                response["publication"]
            ))
        })?;
    let lumvise_db_core::SemanticRevisionPublicationOutcome::Accepted(receipt) = outcome else {
        return Err(revisions::source_failure(format!(
            "source publication conflicted: {outcome:?}"
        )));
    };
    // An accepted child invocation may have consumed the preceding host step budget.
    let acknowledgement = InvocationControl::sixty_seconds();
    state
        .prepared
        .as_ref()
        .ok_or_else(|| import_state_unavailable("prepared import"))?
        .session
        .source_checkout
        .as_ref()
        .ok_or_else(|| import_state_unavailable("source checkout"))?
        .acknowledge(&receipt, &acknowledgement)
        .map_err(revisions::source_failure)?;
    Ok(true)
}

fn acknowledge_import_job(
    service: &Arc<ProjectFolderImportService>,
    job: &ImportJob,
    state: &mut ImportState,
    execution: &ImportExecution<'_>,
) -> Result<(), HttpResponse> {
    let mut prepared = state
        .prepared
        .take()
        .ok_or_else(|| import_state_unavailable("prepared import"))?;
    let Some(scan) = prepared.scan.take() else {
        // A lost reply replays the frozen write first, then scans current files.
        return spawn_import_preparation(service, job, state, prepared.session);
    };
    prepared
        .session
        .indexer
        .commit(scan)
        .map_err(service::scan_failure)?;
    prepared
        .session
        .complete_refresh_attempt(&Ok(Value::Null), Instant::now(), &job.route);
    let elements = state
        .frozen
        .as_ref()
        .map_or(0, |frozen| frozen.batch.semantic_elements.len());
    let relationships = state
        .frozen
        .as_ref()
        .map_or(0, |frozen| frozen.batch.semantic_relationships.len());
    let mut result = service::finished(
        &job.route,
        prepared.files_scanned,
        elements,
        relationships,
        prepared.skipped,
        &job.started_at,
    );
    result["failures"] = json!(
        prepared
            .failures
            .iter()
            .map(|error| json!({"path":error.path,"reason":error.reason}))
            .collect::<Vec<_>>()
    );
    service
        .sessions
        .lock()
        .map_err(|_| import_state_unavailable("import sessions"))?
        .insert((job.root.clone(), job.route.clone()), prepared.session);
    state.result = Some(result);
    state.frozen = None;
    if state.log_started {
        let _ = record_index_log(
            execution,
            json!({"provider_instance_id":PROVIDER_INSTANCE_ID,"plugin_id":"builtin.semantic",
            "project_root":job.route,"index_log_id":job.id,"status":"completed","started_at":job.started_at,
            "completed_at":Utc::now().to_rfc3339(),"metrics":{"filesScanned":prepared.files_scanned,
                "elementsUpserted":elements,"relationshipsUpserted":relationships,"failures":prepared.failures.len()}}),
        );
    }
    Ok(())
}

fn cancel_import_preparation(job: &ImportJob, state: &mut ImportState) -> Result<(), HttpResponse> {
    if state.terminal() {
        return Ok(());
    }
    if state.frozen.is_some() {
        return Err(error_response(
            "409 Conflict",
            "import is committing; expected preparation phase for cancellation",
        ));
    }
    job.cancel.store(true, Ordering::Release);
    state.canceled = true;
    state.receiver = None;
    Ok(())
}

fn import_job_status(job: &ImportJob, state: &ImportState) -> Value {
    let metrics = job
        .progress
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    json!({"importId":job.id,"projectRoot":job.route,"phase":state.phase(),
        "canCancel":!state.terminal() && state.frozen.is_none(), "filesScanned":metrics.files_read,
        "filesParsed":metrics.files_parsed,"entriesInspected":metrics.entries_inspected,
        "pagesPublished":state.next_page,"result":state.result,"error":state.error})
}

fn import_state_unavailable(owner: &str) -> HttpResponse {
    error_response(
        "500 Internal Server Error",
        format!("{owner}; expected available import state"),
    )
}

fn import_failure_message(response: &HttpResponse) -> String {
    let bytes = response.buffered_bytes().unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_slice(bytes).ok();
    parsed
        .as_ref()
        .and_then(|value| value["error"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

pub(super) fn schedule_import_publication(
    service: Arc<ProjectFolderImportService>,
    input: FolderImportInput,
    owner: String,
    publisher: Arc<dyn ImportPublicationPort>,
) -> Result<(), HttpResponse> {
    let id = input
        .import_id
        .as_deref()
        .ok_or_else(|| import_state_unavailable("import identity"))?;
    let root = service::canonical_root(&input.folder_path)?;
    let route = input
        .project_root
        .clone()
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let job = authorized_import_job(&service, id, &root, &route, &owner)?;
    if job.advancing.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let queued = job.clone();
    std::thread::Builder::new()
        .name("project-index-publication".into())
        .spawn(move || {
            let _lease = PublicationStepLease(queued);
            let control = InvocationControl::sixty_seconds();
            if let Err(error) = publisher.authorize(&control) {
                if let Ok(mut state) = _lease.0.state.lock() {
                    state.error = Some(error.to_string());
                }
                return;
            }
            let execution = ImportExecution {
                control: &control,
                nested: publisher.as_ref(),
            };
            let _ = import_job_request(&service, input, &owner, &execution);
        })
        .map_err(|error| {
            job.advancing.store(false, Ordering::Release);
            error_response(
                "500 Internal Server Error",
                format!("cannot schedule index publication: {error}"),
            )
        })?;
    Ok(())
}

struct PublicationStepLease(Arc<ImportJob>);
impl Drop for PublicationStepLease {
    fn drop(&mut self) {
        self.0.advancing.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn importer_scan_emits_unchanged_invalidated_file_through_the_real_projection() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(root.path().join("b.rs"), "fn b() {}\n").unwrap();
        let converter = Arc::new(lumvise_project_indexer::DocumentConverter::default());
        let mut session = super::super::service::import_session(root.path(), converter)
            .ok()
            .expect("import session opens for the temporary source root");
        let initial = session.indexer.prepare(ScanScope::Full).unwrap();
        session.indexer.commit(initial).unwrap();

        let route = root.path().to_string_lossy().into_owned();
        let local = Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap());
        let checkout = crate::index_publication::IndexSourceCheckout::open(
            local.clone(),
            local,
            "import-recovery-test",
            root.path(),
            &route,
            PROVIDER_INSTANCE_ID,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
        let mut before = checkout
            .before_scan(&InvocationControl::sixty_seconds())
            .unwrap();
        // Header discovery is covered by source-checkout tests; isolate this importer handoff.
        before.invalidated_paths.insert("a.rs".into());
        before.requires_full_scan = true;
        session.projection = SemanticIndexProjection::with_source(
            root.path(),
            PROVIDER_INSTANCE_ID,
            checkout.source().clone(),
        )
        .unwrap();
        session.source_checkout = Some(checkout);
        let input = FolderImportInput {
            folder_path: route.clone(),
            project_root: Some(route.clone()),
            replace_paths: Some(vec!["a.rs".into()]),
            import_id: None,
            cancel: false,
            status_only: false,
            existing_only: false,
        };
        let scope = import_scan_scope(&session, &input, &before);
        assert_eq!(scope, ScanScope::Full);
        let prepared = prepare_import_projection(
            session,
            before,
            scope,
            &route,
            root.path(),
            &AtomicBool::new(false),
            &Mutex::new(ScanMetrics::default()),
        )
        .ok()
        .expect("invalidated source path projects successfully");
        let scan = prepared
            .scan
            .expect("successful source preparation retains its scan");
        assert_eq!(scan.metrics().files_read, 1);
        assert_eq!(scan.metrics().files_parsed, 0);
        let batch = prepared.batch.expect("forced scan should produce a batch");
        let emitted_paths = batch
            .semantic_elements
            .iter()
            .map(|element| element.path.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(emitted_paths, BTreeSet::from(["a.rs".to_owned()]));
    }
}
