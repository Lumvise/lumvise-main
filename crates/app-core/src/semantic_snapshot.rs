//! App Core-owned serialized semantic PZ snapshot operations.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use lumvise_db_core::{
    DbError, PzFailurePhase, PzSnapshotResult, SemanticOperation, SemanticPersistence,
    SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppCoreError, Result};
const SNAPSHOT_BACKGROUND_DEADLINE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Lifecycle status of one semantic PZ snapshot operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SemanticSnapshotStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Failure phase for a semantic PZ snapshot operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SemanticSnapshotFailurePhase {
    Capture,
    Encode,
    Validate,
    Publish,
    Cancellation,
}

/// Failure reported by a semantic PZ snapshot operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticSnapshotFailure {
    pub phase: SemanticSnapshotFailurePhase,
    pub message: String,
}

/// Public operation status and optional complete result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticSnapshotOperation {
    pub operation_id: String,
    pub project_id: Option<String>,
    pub project_root: String,
    pub status: SemanticSnapshotStatus,
    pub result: Option<PzSnapshotResult>,
    pub failure: Option<SemanticSnapshotFailure>,
}

struct Job {
    operation_id: String,
    project_root: String,
    output_path: PathBuf,
    control: InvocationControl,
}

const MAX_TERMINAL_OPERATIONS: usize = 64;

struct State {
    operations: HashMap<String, SemanticSnapshotOperation>,
    controls: HashMap<String, InvocationControl>,
    terminal_operations: VecDeque<String>,
}
struct SnapshotWorker {
    semantic: Arc<dyn SemanticPersistence>,
    state: Arc<Mutex<State>>,
    sender: Option<mpsc::Sender<Job>>,
    join_handle: Option<thread::JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
}

/// Serialized App Core worker for complete PZ snapshot publication.
#[derive(Clone)]
pub struct SemanticSnapshotService {
    inner: Arc<SnapshotWorker>,
}
impl SemanticSnapshotService {
    pub(crate) fn new(semantic: Arc<dyn SemanticPersistence>) -> Self {
        let state = Arc::new(Mutex::new(State {
            operations: HashMap::new(),
            controls: HashMap::new(),
            terminal_operations: VecDeque::new(),
        }));
        let (sender, receiver) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_semantic = Arc::clone(&semantic);
        let worker_stopping = Arc::clone(&stopping);
        let join_handle = thread::Builder::new()
            .name("semantic-pz-snapshot".into())
            .spawn(move || worker_loop(worker_semantic, worker_state, receiver, worker_stopping))
            .expect("semantic snapshot worker must start");
        let worker = SnapshotWorker {
            semantic,
            state,
            sender: Some(sender),
            join_handle: Some(join_handle),
            stopping,
        };
        Self {
            inner: Arc::new(worker),
        }
    }

    pub fn create(&self) -> Result<SemanticSnapshotOperation> {
        let lookup_control = InvocationControl::sixty_seconds();
        ensure_control_active(&lookup_control)?;
        self.create_for_selected_project(None, &lookup_control)
    }

    /// Exports the caller's selected project even when several projects are indexed.
    /// Example: `service.create_for_project("/work/micrograd")`.
    pub(crate) fn create_for_project(
        &self,
        project_root: &str,
    ) -> Result<SemanticSnapshotOperation> {
        if project_root.trim().is_empty() {
            return Err(AppCoreError::invalid_value(
                project_root,
                "non-empty project root",
            ));
        }
        self.create_for_selected_project(Some(project_root), &InvocationControl::sixty_seconds())
    }

    fn create_for_selected_project(
        &self,
        selected_project: Option<&str>,
        lookup_control: &InvocationControl,
    ) -> Result<SemanticSnapshotOperation> {
        ensure_control_active(lookup_control)?;
        let project_root = match selected_project
            .map(str::trim)
            .filter(|root| !root.is_empty())
        {
            Some(root) => normalize_project_root(root),
            None => match self
                .inner
                .semantic
                .execute(SemanticOperation::ProjectRoots, lookup_control)?
            {
                SemanticResult::ProjectRoots(roots) if roots.len() == 1 => {
                    normalize_project_root(&roots[0])
                }
                SemanticResult::ProjectRoots(roots) if roots.is_empty() => {
                    return Err(AppCoreError::invalid_value(
                        "",
                        "one active semantic project root",
                    ));
                }
                SemanticResult::ProjectRoots(roots) => {
                    return Err(AppCoreError::invalid_value(
                        roots.join(","),
                        "one active semantic project root",
                    ));
                }
                result => {
                    return Err(AppCoreError::invalid_value(
                        format!("{result:?}"),
                        "project roots",
                    ));
                }
            },
        };
        self.enqueue(
            &project_root,
            InvocationControl::with_deadline(SNAPSHOT_BACKGROUND_DEADLINE),
        )
    }

    fn enqueue(
        &self,
        project_root: &str,
        control: InvocationControl,
    ) -> Result<SemanticSnapshotOperation> {
        ensure_control_active(&control)?;
        let project_root = normalize_project_root(project_root);
        if project_root.is_empty() {
            return Err(AppCoreError::invalid_value(
                project_root,
                "non-empty project root",
            ));
        }
        let operation_id = format!("snapshot-operation-{}", Uuid::now_v7());
        let operation = SemanticSnapshotOperation {
            operation_id: operation_id.clone(),
            project_id: None,
            project_root: project_root.clone(),
            status: SemanticSnapshotStatus::Queued,
            result: None,
            failure: None,
        };
        let output_path = PathBuf::from(&project_root)
            .join(".lumvise")
            .join("graph_db.pz");
        let job = Job {
            operation_id: operation_id.clone(),
            project_root,
            output_path,
            control: control.clone(),
        };
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| AppCoreError::poisoned_mutex("semantic snapshot state"))?;
            state
                .operations
                .insert(operation_id.clone(), operation.clone());
            state.controls.insert(operation_id.clone(), control);
        }
        let sent = match self.inner.sender.as_ref() {
            Some(sender) => sender.send(job).is_ok(),
            None => false,
        };
        if !sent {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| AppCoreError::poisoned_mutex("semantic snapshot state"))?;
            if let Some(operation) = state.operations.get_mut(&operation_id) {
                operation.status = SemanticSnapshotStatus::Failed;
                operation.failure = Some(SemanticSnapshotFailure {
                    phase: SemanticSnapshotFailurePhase::Capture,
                    message: "snapshot worker unavailable".into(),
                });
            }
            mark_terminal(&mut state, &operation_id);
            return state
                .operations
                .get(&operation_id)
                .cloned()
                .ok_or_else(|| AppCoreError::invalid_value(operation_id, "retained operation"));
        }
        Ok(operation)
    }

    /// Returns the current status/result for one operation.
    pub fn status(&self, operation_id: &str) -> Result<SemanticSnapshotOperation> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("semantic snapshot state"))?;
        state.operations.get(operation_id).cloned().ok_or_else(|| {
            AppCoreError::invalid_value(operation_id, "known semantic snapshot operation")
        })
    }

    /// Requests cancellation. Queued work never starts; running work observes the shared control.
    pub fn cancel(&self, operation_id: &str) -> Result<SemanticSnapshotOperation> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AppCoreError::poisoned_mutex("semantic snapshot state"))?;
        let control = state.controls.get(operation_id).cloned();
        let status = state
            .operations
            .get(operation_id)
            .map(|operation| operation.status);
        let Some(status) = status else {
            return Err(AppCoreError::invalid_value(
                operation_id,
                "known semantic snapshot operation",
            ));
        };
        match status {
            SemanticSnapshotStatus::Queued => {
                if let Some(operation) = state.operations.get_mut(operation_id) {
                    operation.status = SemanticSnapshotStatus::Cancelled;
                    operation.failure = Some(SemanticSnapshotFailure {
                        phase: SemanticSnapshotFailurePhase::Cancellation,
                        message: "cancelled before capture".into(),
                    });
                }
                if let Some(control) = control.as_ref() {
                    control.cancel();
                }
                mark_terminal(&mut state, operation_id);
            }
            SemanticSnapshotStatus::Running => {
                if let Some(control) = control.as_ref() {
                    control.cancel();
                }
            }
            SemanticSnapshotStatus::Succeeded
            | SemanticSnapshotStatus::Failed
            | SemanticSnapshotStatus::Cancelled => {}
        }
        state
            .operations
            .get(operation_id)
            .cloned()
            .ok_or_else(|| AppCoreError::invalid_value(operation_id, "retained operation"))
    }
}
fn worker_loop(
    semantic: Arc<dyn SemanticPersistence>,
    state: Arc<Mutex<State>>,
    receiver: mpsc::Receiver<Job>,
    stopping: Arc<AtomicBool>,
) {
    loop {
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let Ok(job) = receiver.recv() else { break };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let should_run = match state.lock() {
            Ok(mut state) => {
                let Some(status) = state
                    .operations
                    .get(&job.operation_id)
                    .map(|operation| operation.status)
                else {
                    continue;
                };
                if status == SemanticSnapshotStatus::Cancelled
                    || job.control.is_cancelled()
                    || job.control.is_expired()
                {
                    if status != SemanticSnapshotStatus::Cancelled {
                        if let Some(operation) = state.operations.get_mut(&job.operation_id) {
                            operation.status = SemanticSnapshotStatus::Cancelled;
                            operation.failure = Some(SemanticSnapshotFailure {
                                phase: SemanticSnapshotFailurePhase::Cancellation,
                                message: "snapshot cancellation requested before capture".into(),
                            });
                        }
                    }
                    mark_terminal(&mut state, &job.operation_id);
                    false
                } else if stopping.load(Ordering::Acquire) {
                    false
                } else {
                    if let Some(operation) = state.operations.get_mut(&job.operation_id) {
                        operation.status = SemanticSnapshotStatus::Running;
                    }
                    true
                }
            }
            Err(_) => false,
        };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        if !should_run {
            continue;
        }
        let result = semantic.execute(
            SemanticOperation::CreatePzSnapshot {
                project_root: job.project_root,
                output_path: job.output_path.to_string_lossy().into_owned(),
            },
            &job.control,
        );
        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => continue,
        };
        {
            let Some(operation) = state.operations.get_mut(&job.operation_id) else {
                continue;
            };
            match result {
                // The DB writer performs the final cancellation check before
                // rename. A successful result therefore owns the commit
                // linearization point even when cancellation follows it.
                Ok(SemanticResult::PzSnapshot(result)) => {
                    operation.project_id = Some(result.project_id.clone());
                    operation.result = Some(result);
                    operation.failure = None;
                    operation.status = SemanticSnapshotStatus::Succeeded;
                }
                Ok(other) => {
                    operation.status = SemanticSnapshotStatus::Failed;
                    operation.failure = Some(SemanticSnapshotFailure {
                        phase: SemanticSnapshotFailurePhase::Capture,
                        message: format!("unexpected semantic result: {other:?}"),
                    });
                }
                Err(error) => {
                    let (phase, message) = app_failure(error, &job.control);
                    operation.status = if phase == SemanticSnapshotFailurePhase::Cancellation {
                        SemanticSnapshotStatus::Cancelled
                    } else {
                        SemanticSnapshotStatus::Failed
                    };
                    operation.failure = Some(SemanticSnapshotFailure { phase, message });
                }
            }
        }
        mark_terminal(&mut state, &job.operation_id);
    }
}

fn ensure_control_active(control: &InvocationControl) -> Result<()> {
    if control.is_cancelled() {
        return Err(AppCoreError::invalid_value(
            "cancelled invocation",
            "active snapshot invocation",
        ));
    }
    if control.is_expired() {
        return Err(AppCoreError::invalid_value(
            "expired invocation",
            "snapshot invocation before deadline",
        ));
    }
    Ok(())
}

fn normalize_project_root(root: &str) -> String {
    let root = root.trim().replace('\\', "/");
    if root == "/" {
        return root;
    }
    root.trim_end_matches('/').to_owned()
}

fn mark_terminal(state: &mut State, operation_id: &str) {
    if !state.operations.contains_key(operation_id) {
        return;
    }
    state.controls.remove(operation_id);
    if !state
        .terminal_operations
        .iter()
        .any(|retained| retained == operation_id)
    {
        state.terminal_operations.push_back(operation_id.to_owned());
    }
    while state.terminal_operations.len() > MAX_TERMINAL_OPERATIONS {
        let Some(evicted) = state.terminal_operations.pop_front() else {
            break;
        };
        state.operations.remove(&evicted);
        state.controls.remove(&evicted);
    }
}

fn app_failure(
    error: DbError,
    control: &InvocationControl,
) -> (SemanticSnapshotFailurePhase, String) {
    match error {
        DbError::Pz { phase, message } => (app_failure_phase(phase), message),
        error if control.is_cancelled() || control.is_expired() => (
            SemanticSnapshotFailurePhase::Cancellation,
            error.to_string(),
        ),
        error => (SemanticSnapshotFailurePhase::Capture, error.to_string()),
    }
}

fn app_failure_phase(phase: PzFailurePhase) -> SemanticSnapshotFailurePhase {
    match phase {
        PzFailurePhase::Capture => SemanticSnapshotFailurePhase::Capture,
        PzFailurePhase::Encode => SemanticSnapshotFailurePhase::Encode,
        PzFailurePhase::Validate => SemanticSnapshotFailurePhase::Validate,
        PzFailurePhase::Publish => SemanticSnapshotFailurePhase::Publish,
        PzFailurePhase::Cancellation => SemanticSnapshotFailurePhase::Cancellation,
    }
}

impl Drop for SnapshotWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        {
            match self.state.lock() {
                Ok(mut state) => mark_shutdown(&mut state),
                Err(poisoned) => mark_shutdown(&mut poisoned.into_inner()),
            }
        }
        drop(self.sender.take());
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

fn mark_shutdown(state: &mut State) {
    for control in state.controls.values() {
        control.cancel();
    }
    let operation_ids = state.operations.keys().cloned().collect::<Vec<_>>();
    for operation_id in operation_ids {
        if let Some(operation) = state.operations.get_mut(&operation_id) {
            match operation.status {
                SemanticSnapshotStatus::Queued => {
                    operation.status = SemanticSnapshotStatus::Cancelled;
                    operation.failure = Some(SemanticSnapshotFailure {
                        phase: SemanticSnapshotFailurePhase::Cancellation,
                        message: "cancelled before capture".into(),
                    });
                }
                SemanticSnapshotStatus::Running => {
                    operation.status = SemanticSnapshotStatus::Cancelled;
                    operation.failure = Some(SemanticSnapshotFailure {
                        phase: SemanticSnapshotFailurePhase::Cancellation,
                        message: "snapshot cancellation requested".into(),
                    });
                }
                SemanticSnapshotStatus::Succeeded
                | SemanticSnapshotStatus::Failed
                | SemanticSnapshotStatus::Cancelled => {}
            }
        }
        mark_terminal(state, &operation_id);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_db_core::{DbError, SemanticReadiness};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakePersistence {
        started: AtomicBool,
    }

    impl SemanticPersistence for FakePersistence {
        fn execute(
            &self,
            operation: SemanticOperation,
            _control: &InvocationControl,
        ) -> std::result::Result<SemanticResult, DbError> {
            match operation {
                SemanticOperation::ProjectRoots => {
                    Ok(SemanticResult::ProjectRoots(vec!["/project".into()]))
                }
                SemanticOperation::CreatePzSnapshot { output_path, .. } => {
                    self.started.store(true, Ordering::Release);
                    Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                        project_id: "project-id".into(),
                        snapshot_id: "snapshot-id".into(),
                        commit_version: 4,
                        published_at: "2026-01-01T00:00:00Z".into(),
                        output_path: output_path.into(),
                        output_bytes: 10,
                        row_counts: Default::default(),
                    }))
                }
                _ => Err(DbError::invalid_value(
                    "operation",
                    "snapshot test operation",
                )),
            }
        }

        fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    struct DropProbePersistence {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for DropProbePersistence {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl SemanticPersistence for DropProbePersistence {
        fn execute(
            &self,
            _operation: SemanticOperation,
            _control: &InvocationControl,
        ) -> std::result::Result<SemanticResult, DbError> {
            Err(DbError::invalid_value(
                "operation",
                "drop probe does not execute",
            ))
        }

        fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    #[test]
    fn final_service_drop_releases_persistence_synchronously() {
        let dropped = Arc::new(AtomicBool::new(false));
        let persistence = Arc::new(DropProbePersistence {
            dropped: Arc::clone(&dropped),
        });
        let service =
            SemanticSnapshotService::new(Arc::clone(&persistence) as Arc<dyn SemanticPersistence>);
        let clone = service.clone();
        drop(persistence);
        drop(service);
        assert!(!dropped.load(Ordering::Acquire));
        drop(clone);
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn create_returns_operation_id_before_worker_result_and_serializes_status() {
        let persistence = Arc::new(FakePersistence {
            started: AtomicBool::new(false),
        });
        let service = SemanticSnapshotService::new(persistence);
        let created = service.create().expect("create snapshot");
        assert!(created.operation_id.starts_with("snapshot-operation-"));
        assert!(matches!(
            created.status,
            SemanticSnapshotStatus::Queued | SemanticSnapshotStatus::Running
        ));
        for _ in 0..100 {
            let status = service.status(&created.operation_id).expect("status");
            if status.status == SemanticSnapshotStatus::Succeeded {
                let json = serde_json::to_value(status).expect("serialize status");
                assert_eq!(json["status"], "succeeded");
                assert_eq!(json["result"]["projectId"], "project-id");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("snapshot worker did not complete");
    }

    struct SlowFakePersistence {
        started: AtomicBool,
    }

    impl SemanticPersistence for SlowFakePersistence {
        fn execute(
            &self,
            operation: SemanticOperation,
            control: &InvocationControl,
        ) -> std::result::Result<SemanticResult, DbError> {
            match operation {
                SemanticOperation::ProjectRoots => {
                    Ok(SemanticResult::ProjectRoots(vec!["/project".into()]))
                }
                SemanticOperation::CreatePzSnapshot { output_path, .. } => {
                    self.started.store(true, Ordering::Release);
                    for _ in 0..500 {
                        if control.is_cancelled() {
                            return Err(DbError::pz(
                                PzFailurePhase::Cancellation,
                                "cancelled before commit",
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                        project_id: "project-id".into(),
                        snapshot_id: "snapshot-id".into(),
                        commit_version: 4,
                        published_at: "2026-01-01T00:00:00Z".into(),
                        output_path: output_path.into(),
                        output_bytes: 10,
                        row_counts: Default::default(),
                    }))
                }
                _ => Err(DbError::invalid_value(
                    "operation",
                    "snapshot test operation",
                )),
            }
        }

        fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    struct CommitThenCancelPersistence;

    impl SemanticPersistence for CommitThenCancelPersistence {
        fn execute(
            &self,
            operation: SemanticOperation,
            control: &InvocationControl,
        ) -> std::result::Result<SemanticResult, DbError> {
            match operation {
                SemanticOperation::ProjectRoots => {
                    Ok(SemanticResult::ProjectRoots(vec!["/project".into()]))
                }
                SemanticOperation::CreatePzSnapshot { output_path, .. } => {
                    control.cancel();
                    Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                        project_id: "project-id".into(),
                        snapshot_id: "committed-snapshot".into(),
                        commit_version: 9,
                        published_at: "2026-01-01T00:00:00Z".into(),
                        output_path: output_path.into(),
                        output_bytes: 12,
                        row_counts: Default::default(),
                    }))
                }
                _ => Err(DbError::invalid_value(
                    "operation",
                    "commit cancellation test operation",
                )),
            }
        }

        fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    #[test]
    fn committed_snapshot_wins_a_late_cancellation_race() {
        let service = SemanticSnapshotService::new(Arc::new(CommitThenCancelPersistence));
        let created = service.create().expect("create snapshot");
        for _ in 0..100 {
            let status = service.status(&created.operation_id).expect("status");
            if status.status == SemanticSnapshotStatus::Succeeded {
                assert_eq!(
                    status.result.expect("committed result").snapshot_id,
                    "committed-snapshot"
                );
                assert!(status.failure.is_none());
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("committed snapshot did not settle as succeeded");
    }

    #[test]
    fn terminal_history_is_bounded_and_releases_controls() {
        let service = SemanticSnapshotService::new(Arc::new(FakePersistence {
            started: AtomicBool::new(false),
        }));
        let mut last = None;
        for _ in 0..=MAX_TERMINAL_OPERATIONS {
            last = Some(service.create().expect("create snapshot"));
        }
        let last = last.expect("last operation");
        for _ in 0..200 {
            if service
                .status(&last.operation_id)
                .expect("last status")
                .status
                == SemanticSnapshotStatus::Succeeded
            {
                let state = service.inner.state.lock().expect("snapshot state");
                assert_eq!(state.operations.len(), MAX_TERMINAL_OPERATIONS);
                assert!(state.controls.is_empty());
                assert_eq!(state.terminal_operations.len(), MAX_TERMINAL_OPERATIONS);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("snapshot history did not settle");
    }

    #[test]
    fn pz_failure_phase_is_preserved() {
        let control = InvocationControl::sixty_seconds();
        let (phase, message) = app_failure(
            DbError::pz(PzFailurePhase::Encode, "encode failed"),
            &control,
        );
        assert_eq!(phase, SemanticSnapshotFailurePhase::Encode);
        assert_eq!(message, "encode failed");
    }

    #[test]
    fn cancelling_a_queued_operation_never_starts_it() {
        let persistence = Arc::new(SlowFakePersistence {
            started: AtomicBool::new(false),
        });
        let service =
            SemanticSnapshotService::new(Arc::clone(&persistence) as Arc<dyn SemanticPersistence>);
        let running = service.create().expect("start first snapshot");
        for _ in 0..100 {
            if service
                .status(&running.operation_id)
                .expect("status")
                .status
                == SemanticSnapshotStatus::Running
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let queued = service.create().expect("enqueue second snapshot");
        assert_eq!(queued.status, SemanticSnapshotStatus::Queued);
        let cancelled = service.cancel(&queued.operation_id).expect("cancel queued");
        assert_eq!(cancelled.status, SemanticSnapshotStatus::Cancelled);
        assert_eq!(
            cancelled.failure.expect("cancellation failure").phase,
            SemanticSnapshotFailurePhase::Cancellation
        );
        service.cancel(&running.operation_id).expect("cancel first");
        for _ in 0..200 {
            let status = service.status(&queued.operation_id).expect("status");
            assert_eq!(status.status, SemanticSnapshotStatus::Cancelled);
            if service
                .status(&running.operation_id)
                .expect("status")
                .status
                != SemanticSnapshotStatus::Running
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("first snapshot did not settle after cancellation");
    }

    #[test]
    fn cancelling_a_running_operation_settles_as_cancelled_and_preserves_no_result() {
        let persistence = Arc::new(SlowFakePersistence {
            started: AtomicBool::new(false),
        });
        let service = SemanticSnapshotService::new(persistence);
        let created = service.create().expect("create snapshot");
        for _ in 0..100 {
            if service
                .status(&created.operation_id)
                .expect("status")
                .status
                == SemanticSnapshotStatus::Running
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        service
            .cancel(&created.operation_id)
            .expect("cancel running");
        for _ in 0..200 {
            let status = service.status(&created.operation_id).expect("status");
            if status.status == SemanticSnapshotStatus::Cancelled {
                assert!(status.result.is_none());
                assert_eq!(
                    status.failure.expect("cancellation failure").phase,
                    SemanticSnapshotFailurePhase::Cancellation
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("running snapshot did not settle as cancelled");
    }
    struct ShutdownPersistence {
        started: std::sync::mpsc::Sender<()>,
        cancelled: std::sync::mpsc::Sender<()>,
        starts: AtomicUsize,
    }

    impl SemanticPersistence for ShutdownPersistence {
        fn execute(
            &self,
            operation: SemanticOperation,
            control: &InvocationControl,
        ) -> std::result::Result<SemanticResult, DbError> {
            match operation {
                SemanticOperation::CreatePzSnapshot { output_path, .. } => {
                    self.starts.fetch_add(1, Ordering::SeqCst);
                    self.started.send(()).expect("started receiver");
                    while !control.is_cancelled() {
                        std::thread::yield_now();
                    }
                    self.cancelled.send(()).expect("cancelled receiver");
                    Ok(SemanticResult::PzSnapshot(PzSnapshotResult {
                        project_id: "project-id".into(),
                        snapshot_id: "snapshot-id".into(),
                        commit_version: 4,
                        published_at: "2026-01-01T00:00:00Z".into(),
                        output_path: output_path.into(),
                        output_bytes: 10,
                        row_counts: Default::default(),
                    }))
                }
                _ => Err(DbError::invalid_value(
                    "operation",
                    "shutdown test operation",
                )),
            }
        }

        fn readiness(&self) -> std::result::Result<SemanticReadiness, DbError> {
            Ok(SemanticReadiness { ready: true })
        }
    }

    #[test]
    fn final_service_drop_cancels_running_work_and_discards_queued_work() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let persistence = Arc::new(ShutdownPersistence {
            started: started_tx,
            cancelled: cancelled_tx,
            starts: AtomicUsize::new(0),
        });
        let service =
            SemanticSnapshotService::new(Arc::clone(&persistence) as Arc<dyn SemanticPersistence>);
        service
            .enqueue("/project", InvocationControl::sixty_seconds())
            .expect("start first snapshot");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first snapshot started");
        service
            .enqueue("/project", InvocationControl::sixty_seconds())
            .expect("enqueue second snapshot");
        drop(service);
        cancelled_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("running snapshot observed cancellation");
        assert_eq!(persistence.starts.load(Ordering::Acquire), 1);
    }
}
