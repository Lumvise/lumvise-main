//! Project-scoped execution broker owned by AppCore.
//!
//! Other AppCore consumers may submit predefined capabilities. Provider
//! transport, connection identifiers, and execution policy remain internal to
//! the registered provider boundary.

mod activity;
mod local;
mod removal;
mod semantic_artifact;
pub use semantic_artifact::SemanticArtifactTask;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_ACTIVE_JOBS: usize = 256;
const MAX_RETAINED_JOBS: usize = 1_024;
/// Hard cap for terminal jobs whose requester has not yet read the final
/// status; past this point the oldest terminal job is evicted even unread so
/// the broker stays bounded when requesters abandon results.
const MAX_UNDELIVERED_RETAINED: usize = 16_384;
const MAX_INPUT_BYTES: usize = 256 * 1024;
const PROVIDER_COMMAND_CAPACITY: usize = 32;
const PROVIDER_LEASE_DURATION: Duration = Duration::from_secs(10);

pub const PROJECT_EXECUTION_REGISTER_ENDPOINT: &str = "/api/project-execution/providers/register";
pub const PROJECT_EXECUTION_NEXT_ENDPOINT: &str = "/api/project-execution/providers/next";
pub const PROJECT_EXECUTION_PROGRESS_ENDPOINT: &str = "/api/project-execution/providers/progress";
pub const PROJECT_EXECUTION_RESULT_ENDPOINT: &str = "/api/project-execution/providers/result";
pub const PROJECT_EXECUTION_HEARTBEAT_ENDPOINT: &str = "/api/project-execution/providers/heartbeat";
pub const PROJECT_EXECUTION_UNREGISTER_ENDPOINT: &str =
    "/api/project-execution/providers/unregister";

/// A transport-neutral request for one predefined project operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectExecutionRequest {
    pub requester_id: String,
    pub project_root: String,
    pub capability_id: String,
    pub idempotency_key: String,
    pub input: Value,
    /// Interactive jobs jump ahead of background jobs in the local queue.
    #[serde(default)]
    pub priority: ProjectExecutionPriority,
}

/// Scheduling class for one project execution request.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectExecutionPriority {
    /// User-initiated work that must not starve behind background refreshes.
    Interactive,
    #[default]
    Background,
}

/// Work delivered to the MCP provider registered for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProjectExecutionCommand {
    Execute {
        job_id: String,
        capability_id: String,
        input: Value,
    },
    Cancel {
        job_id: String,
    },
}

/// Lifecycle state of a project execution job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectExecutionStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Public snapshot returned to AppCore consumers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectExecutionJob {
    pub job_id: String,
    pub requester_id: String,
    pub project_root: String,
    pub capability_id: String,
    pub status: ProjectExecutionStatus,
    pub progress: Option<String>,
    pub output: Option<Value>,
    pub error: Option<String>,
}

/// Errors produced by the AppCore project-execution boundary.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectExecutionError {
    #[error("invalid project execution request `{value}`; expected {expected}")]
    InvalidRequest { value: String, expected: String },
    #[error("no connected provider for project `{project_root}` and capability `{capability_id}`")]
    ProviderUnavailable {
        project_root: String,
        capability_id: String,
    },
    #[error("project execution job `{job_id}` was not found")]
    JobNotFound { job_id: String },
    #[error(
        "project execution job `{job_id}` is owned by requester `{owner}`; expected `{requester_id}`"
    )]
    RequesterMismatch {
        job_id: String,
        owner: String,
        requester_id: String,
    },
    #[error("project execution state is unavailable; expected usable mutex")]
    StateUnavailable,
    #[error("project execution capacity `{actual}` exceeds maximum `{maximum}`")]
    CapacityExceeded { actual: usize, maximum: usize },
    #[error("project execution provider queue `{provider_id}` is full at maximum `{maximum}`")]
    ProviderQueueFull { provider_id: String, maximum: usize },
    #[error("project execution job `{job_id}` is terminal in state `{status:?}`")]
    TerminalJob {
        job_id: String,
        status: ProjectExecutionStatus,
    },
}

/// AppCore-owned registry for project providers and bounded execution jobs.
#[derive(Clone, Default)]
pub struct ProjectExecutionService {
    state: Arc<Mutex<ProjectExecutionState>>,
}

#[derive(Default)]
struct ProjectExecutionState {
    activity: Arc<crate::workspace_activity::WorkspaceActivity>,
    providers: HashMap<String, RegisteredProvider>,
    local_executor: Option<Arc<local::LocalArtifactExecutor>>,
    local_jobs: HashMap<String, local::LocalArtifactControl>,
    jobs: HashMap<String, ProjectExecutionJob>,
    job_inputs: HashMap<String, Value>,
    idempotency: HashMap<(String, String, String, String), String>,
    /// Terminal job ids in the order they became terminal.
    terminal_order: VecDeque<String>,
    /// Terminal jobs whose status was returned to the requester at least once.
    delivered: HashSet<String>,
}

struct RegisteredProvider {
    provider_id: String,
    capabilities: BTreeSet<String>,
    commands: mpsc::SyncSender<ProjectExecutionCommand>,
    lease_deadline: Instant,
}

pub(crate) struct ProjectExecutionControlTransport {
    connections: Mutex<HashMap<String, ProviderConnection>>,
}

#[derive(Clone)]
struct ProviderConnection {
    connection_token: String,
    project_root: String,
    commands: Arc<Mutex<mpsc::Receiver<ProjectExecutionCommand>>>,
}

impl Default for ProjectExecutionControlTransport {
    fn default() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
        }
    }
}

impl ProjectExecutionControlTransport {
    pub(crate) fn register(
        &self,
        service: &ProjectExecutionService,
        provider_id: &str,
        project_root: &str,
        capabilities: Vec<String>,
    ) -> Result<String, ProjectExecutionError> {
        validate_identifier(provider_id, "non-empty provider identifier")?;
        let connection_token = new_opaque_id("project-provider")?;
        let (commands, receiver) = mpsc::sync_channel(PROVIDER_COMMAND_CAPACITY);
        service.register_provider(&connection_token, project_root, capabilities, commands)?;
        self.lock_connections()?.insert(
            provider_id.to_string(),
            ProviderConnection {
                connection_token: connection_token.clone(),
                project_root: normalized_project_root(project_root)?,
                commands: Arc::new(Mutex::new(receiver)),
            },
        );
        Ok(connection_token)
    }

    pub(crate) fn next(
        &self,
        service: &ProjectExecutionService,
        provider_id: &str,
        connection_token: &str,
        timeout: Duration,
    ) -> Result<Option<ProjectExecutionCommand>, ProjectExecutionError> {
        let connection = {
            let connections = self.lock_connections()?;
            authenticated_connection(&connections, provider_id, connection_token)?.clone()
        };
        service.heartbeat(&connection.connection_token)?;
        let commands = connection
            .commands
            .lock()
            .map_err(|_| ProjectExecutionError::StateUnavailable)?;
        match commands.recv_timeout(timeout) {
            Ok(command) => Ok(Some(command)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(provider_unavailable(
                &connection.project_root,
                "connected provider command channel",
            )),
        }
    }

    pub(crate) fn authenticated_provider(
        &self,
        provider_id: &str,
        connection_token: &str,
    ) -> Result<String, ProjectExecutionError> {
        let connections = self.lock_connections()?;
        authenticated_connection(&connections, provider_id, connection_token)
            .map(|connection| connection.connection_token.clone())
    }

    pub(crate) fn unregister(
        &self,
        service: &ProjectExecutionService,
        provider_id: &str,
        connection_token: &str,
    ) -> Result<bool, ProjectExecutionError> {
        let connection = {
            let mut connections = self.lock_connections()?;
            let connection =
                authenticated_connection(&connections, provider_id, connection_token)?.clone();
            connections.remove(provider_id);
            connection
        };
        service.unregister_provider(&connection.connection_token, &connection.project_root)
    }

    fn lock_connections(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, ProviderConnection>>, ProjectExecutionError>
    {
        self.connections
            .lock()
            .map_err(|_| ProjectExecutionError::StateUnavailable)
    }
}

impl ProjectExecutionService {
    /// Reports whether a live provider advertises one exact project capability.
    pub fn provider_available(
        &self,
        project_root: &str,
        capability_id: &str,
    ) -> Result<bool, ProjectExecutionError> {
        let project_root = normalized_project_root(project_root)?;
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, Instant::now());
        Ok(matching_provider(&state, &project_root, capability_id).is_ok())
    }

    /// Registers the live provider for one exact project root.
    ///
    /// A later registration replaces the previous connection for that project.
    pub fn register_provider(
        &self,
        provider_id: &str,
        project_root: &str,
        capabilities: impl IntoIterator<Item = String>,
        commands: mpsc::SyncSender<ProjectExecutionCommand>,
    ) -> Result<(), ProjectExecutionError> {
        validate_identifier(provider_id, "non-empty provider identifier")?;
        let project_root = normalized_project_root(project_root)?;
        let capabilities = validated_capabilities(capabilities)?;
        let provider = RegisteredProvider {
            provider_id: provider_id.to_string(),
            capabilities,
            commands,
            lease_deadline: Instant::now() + PROVIDER_LEASE_DURATION,
        };
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, Instant::now());
        redispatch_active_jobs(&state, &provider, &project_root)?;
        state.providers.insert(project_root, provider);
        Ok(())
    }

    /// Removes a provider only when the live connection identifier matches.
    pub fn unregister_provider(
        &self,
        provider_id: &str,
        project_root: &str,
    ) -> Result<bool, ProjectExecutionError> {
        let project_root = normalized_project_root(project_root)?;
        let mut state = self.lock_state()?;
        let matches = state
            .providers
            .get(&project_root)
            .is_some_and(|provider| provider.provider_id == provider_id);
        if matches {
            state.providers.remove(&project_root);
        }
        Ok(matches)
    }

    /// Submits one predefined capability to the connected project provider.
    pub fn submit(
        &self,
        request: ProjectExecutionRequest,
    ) -> Result<ProjectExecutionJob, ProjectExecutionError> {
        let request = validate_request(request)?;
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, Instant::now());
        if let Some(job) = idempotent_job(&state, &request) {
            if !retryable_terminal(&job) {
                return Ok(job);
            }
            remove_job(&mut state, &job.job_id);
        }
        enforce_capacity(&state)?;
        discard_terminal_jobs(&mut state);
        let job = new_job(&request)?;
        match dispatch_execute(&state, &request, &job) {
            Err(ProjectExecutionError::ProviderUnavailable { .. }) => {
                self.dispatch_local(&mut state, &request, &job)?;
                return Ok(job);
            }
            result => result?,
        }
        store_job(&mut state, &request, &job);
        Ok(job)
    }

    /// Returns a job only to its original requester.
    pub fn status(
        &self,
        requester_id: &str,
        job_id: &str,
    ) -> Result<ProjectExecutionJob, ProjectExecutionError> {
        let mut state = self.lock_state()?;
        let job = owned_job(&state, requester_id, job_id)?.clone();
        if !matches!(
            job.status,
            ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
        ) {
            state.delivered.insert(job_id.to_owned());
        }
        Ok(job)
    }

    /// Requests cancellation from the same provider connection.
    pub fn cancel(
        &self,
        requester_id: &str,
        job_id: &str,
    ) -> Result<ProjectExecutionJob, ProjectExecutionError> {
        let mut state = self.lock_state()?;
        let job = owned_job(&state, requester_id, job_id)?.clone();
        if let Some(control) = state.local_jobs.get(job_id) {
            control.cancel();
        } else {
            let _ = dispatch_cancel(&state, &job);
        }
        let job = state
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| job_missing(job_id))?;
        let was_active = matches!(
            job.status,
            ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
        );
        job.status = ProjectExecutionStatus::Cancelled;
        let cancelled = job.clone();
        if was_active {
            mark_terminal(&mut state, job_id);
        }
        state.job_inputs.remove(job_id);
        Ok(cancelled)
    }

    /// Records provider progress for a known job.
    pub fn record_progress(
        &self,
        provider_id: &str,
        job_id: &str,
        progress: String,
    ) -> Result<ProjectExecutionJob, ProjectExecutionError> {
        let mut state = self.lock_state()?;
        require_job_provider(&state, provider_id, job_id)?;
        require_active_job(&state, job_id)?;
        let job = state
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| job_missing(job_id))?;
        job.status = ProjectExecutionStatus::Running;
        job.progress = Some(progress);
        let running = job.clone();
        state.publish_activity(job_id);
        Ok(running)
    }

    /// Records the terminal provider result for a known job.
    pub fn record_result(
        &self,
        provider_id: &str,
        job_id: &str,
        result: Result<Value, String>,
    ) -> Result<ProjectExecutionJob, ProjectExecutionError> {
        let mut state = self.lock_state()?;
        require_job_provider(&state, provider_id, job_id)?;
        require_active_job(&state, job_id)?;
        let job = state
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| job_missing(job_id))?;
        apply_result(job, result);
        let completed = job.clone();
        mark_terminal(&mut state, job_id);
        state.job_inputs.remove(job_id);
        Ok(completed)
    }

    /// Renews the matching provider lease after authenticated control traffic.
    pub fn heartbeat(&self, provider_id: &str) -> Result<(), ProjectExecutionError> {
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, Instant::now());
        let Some(provider) = state
            .providers
            .values_mut()
            .find(|provider| provider.provider_id == provider_id)
        else {
            return Err(provider_unavailable(
                "unknown project",
                "provider heartbeat",
            ));
        };
        provider.lease_deadline = Instant::now() + PROVIDER_LEASE_DURATION;
        Ok(())
    }

    /// Expires idle provider leases and terminalizes their active jobs.
    pub fn expire_provider_leases(&self) -> Result<(), ProjectExecutionError> {
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, Instant::now());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn expire_provider_leases_at(
        &self,
        now: Instant,
    ) -> Result<(), ProjectExecutionError> {
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, now);
        Ok(())
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ProjectExecutionState>, ProjectExecutionError> {
        self.state
            .lock()
            .map_err(|_| ProjectExecutionError::StateUnavailable)
    }
}

fn validate_request(
    mut request: ProjectExecutionRequest,
) -> Result<ProjectExecutionRequest, ProjectExecutionError> {
    validate_identifier(&request.requester_id, "non-empty requester identifier")?;
    validate_identifier(&request.capability_id, "predefined capability identifier")?;
    validate_identifier(&request.idempotency_key, "non-empty idempotency key")?;
    request.project_root = normalized_project_root(&request.project_root)?;
    let bytes = serde_json::to_vec(&request.input)
        .map_err(|error| invalid_request(error.to_string(), "serializable JSON input"))?
        .len();
    if bytes > MAX_INPUT_BYTES {
        return Err(invalid_request(
            bytes.to_string(),
            &format!("input no larger than {MAX_INPUT_BYTES} bytes"),
        ));
    }
    Ok(request)
}

fn validated_capabilities(
    capabilities: impl IntoIterator<Item = String>,
) -> Result<BTreeSet<String>, ProjectExecutionError> {
    let capabilities = capabilities.into_iter().collect::<BTreeSet<_>>();
    if capabilities.is_empty()
        || capabilities
            .iter()
            .any(|value| value.trim() != value || value.is_empty())
    {
        return Err(invalid_request(
            format!("{capabilities:?}"),
            "one or more non-empty predefined capability identifiers",
        ));
    }
    Ok(capabilities)
}

fn normalized_project_root(value: &str) -> Result<String, ProjectExecutionError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid_request(value, "non-empty absolute project root"));
    }
    let normalized = if trimmed == "/" {
        trimmed
    } else {
        trimmed.trim_end_matches(['/', '\\'])
    };
    Ok(normalized.to_string())
}

fn validate_identifier(value: &str, expected: &str) -> Result<(), ProjectExecutionError> {
    if !value.is_empty() && value.trim() == value {
        return Ok(());
    }
    Err(invalid_request(value, expected))
}

fn idempotent_job(
    state: &ProjectExecutionState,
    request: &ProjectExecutionRequest,
) -> Option<ProjectExecutionJob> {
    let key = idempotency_key(request);
    state
        .idempotency
        .get(&key)
        .and_then(|job_id| state.jobs.get(job_id))
        .cloned()
}

fn retryable_terminal(job: &ProjectExecutionJob) -> bool {
    matches!(
        job.status,
        ProjectExecutionStatus::Failed | ProjectExecutionStatus::Cancelled
    )
}

fn remove_job(state: &mut ProjectExecutionState, job_id: &str) {
    if let Some(control) = state.local_jobs.remove(job_id) {
        control.cancel();
    }
    state.jobs.remove(job_id);
    state.job_inputs.remove(job_id);
    state.terminal_order.retain(|stored_id| stored_id != job_id);
    state.delivered.remove(job_id);
    state.idempotency.retain(|_, stored_id| stored_id != job_id);
}

fn enforce_capacity(state: &ProjectExecutionState) -> Result<(), ProjectExecutionError> {
    let active = state
        .jobs
        .values()
        .filter(|job| {
            matches!(
                job.status,
                ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
            )
        })
        .count();
    if active < MAX_ACTIVE_JOBS {
        return Ok(());
    }
    Err(ProjectExecutionError::CapacityExceeded {
        actual: active + 1,
        maximum: MAX_ACTIVE_JOBS,
    })
}

/// Evicts the oldest delivered terminal job while `retained_cap` is reached.
/// Undelivered terminal jobs are kept until `undelivered_cap` so a requester
/// never loses a final status it has not read yet unless the broker would grow
/// unboundedly behind abandoned results.
fn discard_terminal_jobs(state: &mut ProjectExecutionState) {
    discard_terminal_jobs_with(state, MAX_RETAINED_JOBS, MAX_UNDELIVERED_RETAINED);
}

fn discard_terminal_jobs_with(
    state: &mut ProjectExecutionState,
    retained_cap: usize,
    undelivered_cap: usize,
) {
    while state.jobs.len() >= retained_cap {
        let evict_id = state
            .terminal_order
            .iter()
            .find(|job_id| state.delivered.contains(*job_id))
            .cloned()
            .or_else(|| {
                (state.jobs.len() >= undelivered_cap)
                    .then(|| state.terminal_order.front().cloned())
                    .flatten()
            });
        let Some(job_id) = evict_id else {
            return;
        };
        remove_job(state, &job_id);
    }
}

/// Records that a job reached a terminal status for retention bookkeeping.
fn mark_terminal(state: &mut ProjectExecutionState, job_id: &str) {
    state.terminal_order.push_back(job_id.to_owned());
    state.publish_activity(job_id);
}

fn new_job(
    request: &ProjectExecutionRequest,
) -> Result<ProjectExecutionJob, ProjectExecutionError> {
    let job_id = new_opaque_id("project-execution")?;
    Ok(ProjectExecutionJob {
        job_id,
        requester_id: request.requester_id.clone(),
        project_root: request.project_root.clone(),
        capability_id: request.capability_id.clone(),
        status: ProjectExecutionStatus::Queued,
        progress: None,
        output: None,
        error: None,
    })
}

fn new_opaque_id(prefix: &str) -> Result<String, ProjectExecutionError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| {
        invalid_request(
            error.to_string(),
            "available secure job identifier generator",
        )
    })?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("{prefix}-{suffix}"))
}

fn dispatch_execute(
    state: &ProjectExecutionState,
    request: &ProjectExecutionRequest,
    job: &ProjectExecutionJob,
) -> Result<(), ProjectExecutionError> {
    let provider = matching_provider(state, &request.project_root, &request.capability_id)?;
    provider
        .commands
        .try_send(execute_command(job, request.input.clone()))
        .map_err(|error| {
            command_dispatch_error(
                error,
                provider,
                &request.project_root,
                &request.capability_id,
            )
        })
}

fn dispatch_cancel(
    state: &ProjectExecutionState,
    job: &ProjectExecutionJob,
) -> Result<(), ProjectExecutionError> {
    let provider = matching_provider(state, &job.project_root, &job.capability_id)?;
    provider
        .commands
        .try_send(ProjectExecutionCommand::Cancel {
            job_id: job.job_id.clone(),
        })
        .map_err(|error| {
            command_dispatch_error(error, provider, &job.project_root, &job.capability_id)
        })
}

fn matching_provider<'a>(
    state: &'a ProjectExecutionState,
    project_root: &str,
    capability_id: &str,
) -> Result<&'a RegisteredProvider, ProjectExecutionError> {
    state
        .providers
        .get(project_root)
        .filter(|provider| provider.capabilities.contains(capability_id))
        .ok_or_else(|| provider_unavailable(project_root, capability_id))
}

fn store_job(
    state: &mut ProjectExecutionState,
    request: &ProjectExecutionRequest,
    job: &ProjectExecutionJob,
) {
    state.record_activity(request, job);
    state
        .job_inputs
        .insert(job.job_id.clone(), request.input.clone());
    state
        .idempotency
        .insert(idempotency_key(request), job.job_id.clone());
    state.jobs.insert(job.job_id.clone(), job.clone());
}

fn redispatch_active_jobs(
    state: &ProjectExecutionState,
    provider: &RegisteredProvider,
    project_root: &str,
) -> Result<(), ProjectExecutionError> {
    for job in state.jobs.values().filter(|job| {
        !state.local_jobs.contains_key(&job.job_id) && reconnectable(job, project_root, provider)
    }) {
        let input = state
            .job_inputs
            .get(&job.job_id)
            .cloned()
            .ok_or_else(|| job_missing(&job.job_id))?;
        provider
            .commands
            .try_send(execute_command(job, input))
            .map_err(|error| {
                command_dispatch_error(error, provider, project_root, &job.capability_id)
            })?;
    }
    Ok(())
}

fn reconnectable(
    job: &ProjectExecutionJob,
    project_root: &str,
    provider: &RegisteredProvider,
) -> bool {
    job.project_root == project_root
        && provider.capabilities.contains(&job.capability_id)
        && matches!(
            job.status,
            ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
        )
}

fn execute_command(job: &ProjectExecutionJob, input: Value) -> ProjectExecutionCommand {
    ProjectExecutionCommand::Execute {
        job_id: job.job_id.clone(),
        capability_id: job.capability_id.clone(),
        input,
    }
}

fn idempotency_key(request: &ProjectExecutionRequest) -> (String, String, String, String) {
    (
        request.requester_id.clone(),
        request.project_root.clone(),
        request.capability_id.clone(),
        request.idempotency_key.clone(),
    )
}

fn owned_job<'a>(
    state: &'a ProjectExecutionState,
    requester_id: &str,
    job_id: &str,
) -> Result<&'a ProjectExecutionJob, ProjectExecutionError> {
    let job = state.jobs.get(job_id).ok_or_else(|| job_missing(job_id))?;
    if job.requester_id == requester_id {
        return Ok(job);
    }
    Err(ProjectExecutionError::RequesterMismatch {
        job_id: job_id.to_string(),
        owner: job.requester_id.clone(),
        requester_id: requester_id.to_string(),
    })
}

fn require_job_provider(
    state: &ProjectExecutionState,
    provider_id: &str,
    job_id: &str,
) -> Result<(), ProjectExecutionError> {
    let job = state.jobs.get(job_id).ok_or_else(|| job_missing(job_id))?;
    if state.local_jobs.contains_key(job_id) {
        return Err(provider_unavailable(&job.project_root, &job.capability_id));
    }
    let provider = matching_provider(state, &job.project_root, &job.capability_id)?;
    if provider.provider_id == provider_id {
        return Ok(());
    }
    Err(provider_unavailable(&job.project_root, &job.capability_id))
}

fn require_active_job(
    state: &ProjectExecutionState,
    job_id: &str,
) -> Result<(), ProjectExecutionError> {
    let job = state.jobs.get(job_id).ok_or_else(|| job_missing(job_id))?;
    if matches!(
        job.status,
        ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
    ) {
        return Ok(());
    }
    Err(ProjectExecutionError::TerminalJob {
        job_id: job_id.to_owned(),
        status: job.status,
    })
}

fn command_dispatch_error(
    error: mpsc::TrySendError<ProjectExecutionCommand>,
    provider: &RegisteredProvider,
    project_root: &str,
    capability_id: &str,
) -> ProjectExecutionError {
    match error {
        mpsc::TrySendError::Full(_) => ProjectExecutionError::ProviderQueueFull {
            provider_id: provider.provider_id.clone(),
            maximum: PROVIDER_COMMAND_CAPACITY,
        },
        mpsc::TrySendError::Disconnected(_) => provider_unavailable(project_root, capability_id),
    }
}

fn expire_provider_leases(state: &mut ProjectExecutionState, now: Instant) {
    let expired_roots = state
        .providers
        .iter()
        .filter(|(_, provider)| provider.lease_deadline <= now)
        .map(|(root, _)| root.clone())
        .collect::<Vec<_>>();
    for project_root in &expired_roots {
        state.providers.remove(project_root);
    }
    let expired_active_jobs = state
        .jobs
        .values()
        .filter(|job| {
            !state.local_jobs.contains_key(&job.job_id)
                && expired_roots.contains(&job.project_root)
                && matches!(
                    job.status,
                    ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
                )
        })
        .map(|job| job.job_id.clone())
        .collect::<Vec<_>>();
    for job_id in &expired_active_jobs {
        let job = state.jobs.get_mut(job_id).expect("filtered active job");
        job.status = ProjectExecutionStatus::Failed;
        job.error = Some("project execution provider lease expired".into());
        mark_terminal(state, job_id);
    }
}

fn apply_result(job: &mut ProjectExecutionJob, result: Result<Value, String>) {
    match result {
        Ok(output) => {
            job.status = ProjectExecutionStatus::Succeeded;
            job.output = Some(output);
            job.error = None;
        }
        Err(error) => {
            job.status = ProjectExecutionStatus::Failed;
            job.output = None;
            job.error = Some(error);
        }
    }
}

fn invalid_request(value: impl Into<String>, expected: &str) -> ProjectExecutionError {
    ProjectExecutionError::InvalidRequest {
        value: value.into(),
        expected: expected.to_string(),
    }
}

fn provider_unavailable(project_root: &str, capability_id: &str) -> ProjectExecutionError {
    ProjectExecutionError::ProviderUnavailable {
        project_root: project_root.to_string(),
        capability_id: capability_id.to_string(),
    }
}

fn job_missing(job_id: &str) -> ProjectExecutionError {
    ProjectExecutionError::JobNotFound {
        job_id: job_id.to_string(),
    }
}

fn authenticated_connection<'a>(
    connections: &'a HashMap<String, ProviderConnection>,
    provider_id: &str,
    connection_token: &str,
) -> Result<&'a ProviderConnection, ProjectExecutionError> {
    let connection = connections
        .get(provider_id)
        .ok_or_else(|| provider_unavailable("unknown project", "registered provider connection"))?;
    if connection.connection_token == connection_token {
        return Ok(connection);
    }
    Err(invalid_request(
        connection_token,
        "connection token issued to the registered provider",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_terminal_jobs_evict_oldest_first_with_idempotency() {
        let mut state = ProjectExecutionState::default();
        for index in 0..MAX_RETAINED_JOBS {
            let job_id = format!("job-{index}");
            state.jobs.insert(job_id.clone(), terminal_job(&job_id));
            mark_terminal(&mut state, &job_id);
            state.delivered.insert(job_id.clone());
            state.idempotency.insert(
                (
                    "caller".into(),
                    "/project".into(),
                    "capability".into(),
                    job_id.clone(),
                ),
                job_id,
            );
        }

        discard_terminal_jobs(&mut state);

        assert_eq!(state.jobs.len(), MAX_RETAINED_JOBS - 1);
        assert_eq!(state.idempotency.len(), MAX_RETAINED_JOBS - 1);
        // Oldest delivered terminal job goes first.
        assert!(!state.jobs.contains_key("job-0"));
        assert!(state.jobs.contains_key("job-1"));
        assert!(
            state
                .jobs
                .contains_key(&format!("job-{}", MAX_RETAINED_JOBS - 1))
        );
    }

    #[test]
    fn undelivered_terminal_jobs_survive_retention_cap_until_hard_cap() {
        let mut state = ProjectExecutionState::default();
        for index in 0..1_100 {
            let job_id = format!("job-{index}");
            state.jobs.insert(job_id.clone(), terminal_job(&job_id));
            mark_terminal(&mut state, &job_id);
        }

        // No delivered terminal job: nothing is evicted below the hard cap.
        discard_terminal_jobs(&mut state);
        assert_eq!(state.jobs.len(), 1_100);
        for index in 0..1_100 {
            assert!(state.jobs.contains_key(&format!("job-{index}")));
        }

        // At the undelivered hard cap the oldest terminal job is evicted.
        discard_terminal_jobs_with(&mut state, 1_100, 1_100);
        assert_eq!(state.jobs.len(), 1_099);
        assert!(!state.jobs.contains_key("job-0"));
        assert!(state.jobs.contains_key("job-1"));
    }

    #[test]
    fn undelivered_failed_jobs_stay_readable_and_evict_after_delivery() {
        let service = ProjectExecutionService::default();
        let (sender, receiver) = mpsc::sync_channel(PROVIDER_COMMAND_CAPACITY);
        let sender = Arc::new(sender);
        service
            .register_provider("provider", "/project", ["run".into()], (*sender).clone())
            .unwrap();
        let mut jobs = Vec::new();
        // 255 per wave keeps under MAX_ACTIVE_JOBS; expiring the lease between
        // waves terminalizes the wave without anyone reading its status.
        for wave in 0..5 {
            let wave_size = if wave == 4 { 80 } else { 255 };
            for _ in 0..wave_size {
                jobs.push(
                    service
                        .submit(request("/project", &format!("job-{}", jobs.len())))
                        .unwrap(),
                );
                receiver
                    .recv_timeout(Duration::from_secs(5))
                    .expect("provider command for each submission");
            }
            service
                .expire_provider_leases_at(Instant::now() + PROVIDER_LEASE_DURATION)
                .unwrap();
            // Expired the lease removed the provider; reconnect for next wave.
            service
                .register_provider("provider", "/project", ["run".into()], (*sender).clone())
                .unwrap();
        }

        // None of the failed jobs is evicted before its status was read.
        for job in &jobs {
            assert_eq!(
                service.status("caller", &job.job_id).unwrap().status,
                ProjectExecutionStatus::Failed
            );
        }

        // At the retention cap the next submission evicts delivered jobs
        // oldest-first (the whole first wave is older than any later wave);
        // within a wave the terminal order follows HashMap iteration, so
        // assert per-wave, not per-index.
        let extra = service.submit(request("/project", "extra")).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("provider command for the extra submission");
        let mut evicted = 0;
        for (index, job) in jobs.iter().enumerate() {
            let gone = service.status("caller", &job.job_id).is_err();
            assert!(
                !gone || index < 255,
                "a wave-2 job should never evict before wave-1 jobs: {index}"
            );
            evicted += usize::from(gone);
        }
        assert_eq!(evicted, 77);
        assert!(service.status("caller", &jobs[255].job_id).is_ok());
        assert_eq!(
            service.status("caller", &extra.job_id).unwrap().status,
            ProjectExecutionStatus::Queued
        );
    }

    #[test]
    fn provider_queue_is_bounded_and_isolated() {
        let service = ProjectExecutionService::default();
        let (full_sender, _full_receiver) = mpsc::sync_channel(PROVIDER_COMMAND_CAPACITY);
        service
            .register_provider("provider-a", "/a", ["run".into()], full_sender)
            .unwrap();
        for index in 0..PROVIDER_COMMAND_CAPACITY {
            service
                .submit(request("/a", &format!("a-{index}")))
                .unwrap();
        }
        assert!(matches!(
            service.submit(request("/a", "overflow")),
            Err(ProjectExecutionError::ProviderQueueFull { .. })
        ));

        let (other_sender, other_receiver) = mpsc::sync_channel(1);
        service
            .register_provider("provider-b", "/b", ["run".into()], other_sender)
            .unwrap();
        service.submit(request("/b", "progress")).unwrap();
        assert!(matches!(
            other_receiver.recv().unwrap(),
            ProjectExecutionCommand::Execute { .. }
        ));
    }

    #[test]
    fn lease_expiry_and_cancellation_are_terminal_against_late_results() {
        let service = ProjectExecutionService::default();
        let (sender, _receiver) = mpsc::sync_channel(4);
        service
            .register_provider("provider", "/project", ["run".into()], sender)
            .unwrap();
        let expired = service.submit(request("/project", "expired")).unwrap();
        service
            .expire_provider_leases_at(Instant::now() + PROVIDER_LEASE_DURATION)
            .unwrap();
        assert_eq!(
            service.status("caller", &expired.job_id).unwrap().status,
            ProjectExecutionStatus::Failed
        );

        let (sender, _receiver) = mpsc::sync_channel(4);
        service
            .register_provider("provider-2", "/project", ["run".into()], sender)
            .unwrap();
        let cancelled = service.submit(request("/project", "cancelled")).unwrap();
        service.cancel("caller", &cancelled.job_id).unwrap();
        assert!(matches!(
            service.record_result("provider-2", &cancelled.job_id, Ok(Value::Null)),
            Err(ProjectExecutionError::TerminalJob { .. })
        ));
    }

    fn request(project_root: &str, key: &str) -> ProjectExecutionRequest {
        ProjectExecutionRequest {
            requester_id: "caller".into(),
            project_root: project_root.into(),
            capability_id: "run".into(),
            idempotency_key: key.into(),
            input: Value::Null,
            priority: ProjectExecutionPriority::default(),
        }
    }

    fn terminal_job(job_id: &str) -> ProjectExecutionJob {
        ProjectExecutionJob {
            job_id: job_id.into(),
            requester_id: "caller".into(),
            project_root: "/project".into(),
            capability_id: "capability".into(),
            status: ProjectExecutionStatus::Succeeded,
            progress: None,
            output: Some(Value::Null),
            error: None,
        }
    }
}
