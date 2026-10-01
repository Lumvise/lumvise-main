//! Local fallback for the fixed artifact capability. Job ownership stays in the broker.
//!
//! Scheduling: one FIFO queue served by on-demand worker threads. A job is
//! admitted only while its provider has a free completion slot, so queue time
//! never consumes a job's deadline and per-provider order is preserved.
//! Transient engine failures retry with backoff before the job fails.
use super::*;
use lumvise_frontend_core::{AssistantEngine, FrontendCore};
use lumvise_neural_core::llm_providers::{
    LlmExecutionControl, LlmExecutorRegistry, LlmFailure, LlmFailureCode, LlmMessage, LlmRequest,
    LlmRequestOptions, LlmResponseFormat,
};
use lumvise_resource_routing::InvocationControl;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Condvar;

const ARTIFACT_CAPABILITY: &str = "semantic.generate_functional_artifacts.v1";
const MAX_LOCAL_WORKERS: usize = 16;
const WORKER_IDLE_EXIT: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 3;
#[cfg(not(test))]
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];
#[cfg(test)]
const RETRY_BACKOFF: [Duration; 2] = [Duration::from_millis(10), Duration::from_millis(20)];
const BACKOFF_SLICE: Duration = Duration::from_millis(50);

/// Per-provider circuit breaker: consecutive transport-class failures that
/// trip a cooldown, so an outage makes queued jobs wait instead of failing.
const BREAKER_THRESHOLD: u32 = 5;
#[cfg(not(test))]
const BREAKER_COOLDOWN_BASE: Duration = Duration::from_secs(5);
#[cfg(test)]
const BREAKER_COOLDOWN_BASE: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const BREAKER_COOLDOWN_MAX: Duration = Duration::from_secs(300);
#[cfg(test)]
const BREAKER_COOLDOWN_MAX: Duration = Duration::from_millis(500);

pub(super) enum LocalArtifactControl {
    Queued,
    Running(InvocationControl),
}

impl LocalArtifactControl {
    pub(super) fn cancel(&self) {
        if let Self::Running(control) = self {
            control.cancel();
        }
    }
}

pub(super) struct LocalArtifactExecutor {
    frontend: Arc<Mutex<FrontendCore>>,
    llms: Arc<LlmExecutorRegistry>,
    queue: Mutex<LocalQueue>,
    changed: Condvar,
}

#[derive(Default)]
struct LocalQueue {
    pending: VecDeque<QueuedArtifact>,
    /// Running jobs per provider, bounded by the provider's concurrency.
    active: HashMap<String, usize>,
    /// Circuit-breaker state per provider.
    health: HashMap<String, ProviderHealth>,
    workers: usize,
    idle: usize,
}

#[derive(Debug, Clone)]
struct ProviderHealth {
    consecutive_transport_failures: u32,
    cooldown_until: Option<Instant>,
    /// Cooldown applied on the next trip; doubles per trip, reset on success.
    cooldown: Duration,
}

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            consecutive_transport_failures: 0,
            cooldown_until: None,
            cooldown: BREAKER_COOLDOWN_BASE,
        }
    }
}

struct QueuedArtifact {
    job_id: String,
    priority: ProjectExecutionPriority,
    prepared: PreparedArtifactExecution,
}

struct PreparedArtifactExecution {
    task: SemanticArtifactTask,
    provider_id: String,
    model: Option<String>,
}

enum AttemptFailure {
    /// Provider rejected structured output; degrade once to prompt-only JSON.
    StructuredRejected(String),
    /// Retryable failure; `transport` marks provider connect/timeout-class
    /// failures that feed the per-provider circuit breaker.
    Transient {
        error: String,
        transport: bool,
    },
    Fatal(String),
}

impl ProjectExecutionService {
    pub(crate) fn with_local_artifacts(
        frontend: Arc<Mutex<FrontendCore>>,
        llms: Arc<LlmExecutorRegistry>,
        activity: Arc<crate::workspace_activity::WorkspaceActivity>,
    ) -> Self {
        let state = ProjectExecutionState {
            activity,
            local_executor: Some(Arc::new(LocalArtifactExecutor {
                frontend,
                llms,
                queue: Mutex::new(LocalQueue::default()),
                changed: Condvar::new(),
            })),
            ..Default::default()
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub(super) fn dispatch_local(
        &self,
        state: &mut ProjectExecutionState,
        request: &ProjectExecutionRequest,
        job: &ProjectExecutionJob,
    ) -> Result<(), ProjectExecutionError> {
        let executor = state
            .local_executor
            .clone()
            .ok_or_else(|| provider_unavailable(&request.project_root, &request.capability_id))?;
        let prepared = executor.prepare(request)?;
        store_job(state, request, job);
        state
            .local_jobs
            .insert(job.job_id.clone(), LocalArtifactControl::Queued);
        let queued = QueuedArtifact {
            job_id: job.job_id.clone(),
            priority: request.priority,
            prepared,
        };
        if let Err(error) = executor.enqueue(self, queued) {
            state.activity.update(
                &job.job_id,
                crate::workspace_activity::ActivityStatus::Failed,
                Some(error.to_string()),
            );
            remove_job(state, &job.job_id);
            return Err(error);
        }
        Ok(())
    }

    fn run_local_worker(&self, executor: Arc<LocalArtifactExecutor>) {
        while let Some(queued) = executor.next_job() {
            let provider_id = queued.prepared.provider_id.clone();
            self.run_local_job(&executor, &queued);
            executor.release(&provider_id);
        }
    }

    fn run_local_job(&self, executor: &LocalArtifactExecutor, queued: &QueuedArtifact) {
        let job_id = queued.job_id.as_str();
        // Degrades once: a provider that rejects structured output is retried
        // prompt-only in the same attempt slot instead of failing the job.
        let mut structured = true;
        let mut attempt = 1_u32;
        loop {
            let control = match self.begin_local_turn(job_id, &queued.prepared.provider_id, attempt)
            {
                Ok(control) => control,
                // Cancelled or otherwise terminal: the engine is never called.
                Err(error) => return self.complete_local(job_id, Err(error)),
            };
            match executor.attempt(&queued.prepared, control, structured) {
                Ok(output) => {
                    executor.report_success(&queued.prepared.provider_id);
                    return self.complete_local(job_id, Ok(output));
                }
                Err(AttemptFailure::StructuredRejected(error)) if structured => {
                    structured = false;
                    tracing::warn!(
                        target: "app-core::project_execution",
                        provider_id = %queued.prepared.provider_id,
                        model = ?queued.prepared.model,
                        job_id = %job_id,
                        target = %queued.prepared.task.target_id(),
                        error = %error,
                        "provider rejected structured output; retrying without response_format"
                    );
                }
                Err(AttemptFailure::Transient { error, transport }) => {
                    if transport {
                        executor.report_transport_failure(&queued.prepared.provider_id);
                    }
                    if attempt < MAX_ATTEMPTS {
                        if !self.wait_retry(job_id, RETRY_BACKOFF[attempt as usize - 1]) {
                            return self.complete_local(job_id, Err(error));
                        }
                        attempt += 1;
                    } else {
                        let error = if attempt > 1 {
                            format!("{error} (after {attempt} attempts)")
                        } else {
                            error
                        };
                        return self.complete_local(job_id, Err(error));
                    }
                }
                Err(AttemptFailure::Fatal(error)) => {
                    let error = if attempt > 1 {
                        format!("{error} (after {attempt} attempts)")
                    } else {
                        error
                    };
                    return self.complete_local(job_id, Err(error));
                }
                // Already-degraded structured rejection: model nondeterminism.
                Err(AttemptFailure::StructuredRejected(error)) => {
                    let error = if attempt > 1 {
                        format!("{error} (after {attempt} attempts)")
                    } else {
                        error
                    };
                    return self.complete_local(job_id, Err(error));
                }
            }
        }
    }

    /// Sleeps before a retry; false when the job was cancelled meanwhile.
    fn wait_retry(&self, job_id: &str, backoff: Duration) -> bool {
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline {
            if !self.local_job_active(job_id) {
                return false;
            }
            std::thread::sleep(BACKOFF_SLICE.min(deadline - Instant::now()));
        }
        self.local_job_active(job_id)
    }

    fn local_job_active(&self, job_id: &str) -> bool {
        self.lock_state()
            .is_ok_and(|state| require_active_job(&state, job_id).is_ok())
    }

    fn begin_local_turn(
        &self,
        job_id: &str,
        provider_id: &str,
        attempt: u32,
    ) -> Result<InvocationControl, String> {
        let mut state = self.lock_state().map_err(|error| error.to_string())?;
        require_active_job(&state, job_id).map_err(|error| error.to_string())?;
        let job = state
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| job_missing(job_id).to_string())?;
        job.status = ProjectExecutionStatus::Running;
        job.progress = Some(if attempt == 1 {
            format!("Generating with {provider_id}")
        } else {
            format!("Generating with {provider_id} (attempt {attempt}/{MAX_ATTEMPTS})")
        });
        // Every attempt receives its own deadline when it is dispatched;
        // cancellation reaches the running attempt through this control.
        let control = InvocationControl::sixty_seconds();
        state.local_jobs.insert(
            job_id.to_owned(),
            LocalArtifactControl::Running(control.clone()),
        );
        state.publish_activity(job_id);
        Ok(control)
    }

    fn complete_local(&self, job_id: &str, result: Result<Value, String>) {
        if let Ok(mut state) = self.lock_state() {
            if require_active_job(&state, job_id).is_ok()
                && let Some(job) = state.jobs.get_mut(job_id)
            {
                match &result {
                    Ok(_) => tracing::debug!(
                        target: "app-core::project_execution",
                        job_id = %job_id, project_root = %job.project_root,
                        "local artifact job succeeded"
                    ),
                    Err(error) => tracing::warn!(
                        target: "app-core::project_execution",
                        job_id = %job_id, project_root = %job.project_root, error = %error,
                        "local artifact job failed"
                    ),
                }
                apply_result(job, result);
                mark_terminal(&mut state, job_id);
            }
            state.local_jobs.remove(job_id);
            state.job_inputs.remove(job_id);
        }
    }
}

impl LocalArtifactExecutor {
    fn prepare(
        &self,
        request: &ProjectExecutionRequest,
    ) -> Result<PreparedArtifactExecution, ProjectExecutionError> {
        if request.capability_id != ARTIFACT_CAPABILITY
            || !Path::new(&request.project_root).is_dir()
        {
            return Err(provider_unavailable(
                &request.project_root,
                &request.capability_id,
            ));
        }
        let (provider_id, model) = self.configured_engine(&request.project_root)?;
        let task =
            SemanticArtifactTask::prepare(request.input.clone(), Path::new(&request.project_root))
                .map_err(|error| invalid_request(error, "valid local semantic artifact task"))?;
        Ok(PreparedArtifactExecution {
            task,
            provider_id,
            model,
        })
    }

    fn configured_engine(
        &self,
        project_root: &str,
    ) -> Result<(String, Option<String>), ProjectExecutionError> {
        let frontend = self
            .frontend
            .lock()
            .map_err(|_| ProjectExecutionError::StateUnavailable)?;
        let settings = frontend.app_settings();
        let (engine, model) = match settings.generation_engine {
            Some(engine) if engine != AssistantEngine::NativeMcp => {
                (engine, settings.generation_model.clone())
            }
            _ => {
                let engine = settings.assistant_engine;
                if engine == AssistantEngine::NativeMcp {
                    return Err(provider_unavailable(
                        project_root,
                        "local artifact generation: select an LLM engine in Settings",
                    ));
                }
                (engine, settings.assistant_model.clone())
            }
        };
        drop(frontend);
        Ok((engine.value().to_owned(), model))
    }

    /// Queues one job and makes sure a worker will pick it up.
    fn enqueue(
        self: &Arc<Self>,
        service: &ProjectExecutionService,
        queued: QueuedArtifact,
    ) -> Result<(), ProjectExecutionError> {
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ProjectExecutionError::StateUnavailable)?;
        queue.pending.push_back(queued);
        if queue.idle == 0 && queue.workers < MAX_LOCAL_WORKERS {
            let service = service.clone();
            let executor = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("lumvise-local-artifacts".into())
                .spawn(move || service.run_local_worker(executor));
            match spawned {
                Ok(_) => queue.workers += 1,
                // Existing workers still drain the queue; only a pool with no
                // worker at all cannot accept the job.
                Err(error) if queue.workers == 0 => {
                    queue.pending.pop_back();
                    return Err(invalid_request(
                        error.to_string(),
                        "available local artifact worker",
                    ));
                }
                Err(_) => {}
            }
        }
        self.changed.notify_all();
        Ok(())
    }

    /// Next job whose provider has a free slot and is not in a breaker
    /// cooldown — interactive jobs first, then background in arrival order —
    /// or `None` once idle long enough.
    fn next_job(&self) -> Option<QueuedArtifact> {
        let mut queue = self.queue.lock().ok()?;
        loop {
            let admissible = |wanted: ProjectExecutionPriority| {
                queue.pending.iter().position(|queued| {
                    queued.priority == wanted
                        && !Self::in_cooldown(&queue.health, &queued.prepared.provider_id)
                        && queue
                            .active
                            .get(&queued.prepared.provider_id)
                            .copied()
                            .unwrap_or(0)
                            < self
                                .llms
                                .completion_concurrency(&queued.prepared.provider_id)
                })
            };
            let index = admissible(ProjectExecutionPriority::Interactive)
                .or_else(|| admissible(ProjectExecutionPriority::Background));
            if let Some(queued) = index.and_then(|index| queue.pending.remove(index)) {
                *queue
                    .active
                    .entry(queued.prepared.provider_id.clone())
                    .or_default() += 1;
                return Some(queued);
            }
            queue.idle += 1;
            // A pending job blocked only by its provider's cooldown waits for
            // the earliest cooldown end instead of the idle timeout; that wait
            // never reaches the idle-exit rule below.
            let now = Instant::now();
            let cooldown_wake = queue
                .pending
                .iter()
                .filter_map(|queued| {
                    let health = queue.health.get(&queued.prepared.provider_id)?;
                    let until = health.cooldown_until?;
                    (until > now
                        && queue
                            .active
                            .get(&queued.prepared.provider_id)
                            .copied()
                            .unwrap_or(0)
                            < self
                                .llms
                                .completion_concurrency(&queued.prepared.provider_id))
                    .then_some(until)
                })
                .min();
            if let Some(until) = cooldown_wake {
                let waited = self
                    .changed
                    .wait_timeout(queue, until.saturating_duration_since(now));
                let Ok((guard, _)) = waited else {
                    return None;
                };
                queue = guard;
                queue.idle -= 1;
                continue;
            }
            let waited = self.changed.wait_timeout(queue, WORKER_IDLE_EXIT);
            let Ok((guard, timeout)) = waited else {
                return None;
            };
            queue = guard;
            queue.idle -= 1;
            if timeout.timed_out() && queue.pending.is_empty() {
                queue.workers -= 1;
                return None;
            }
        }
    }

    fn in_cooldown(health: &HashMap<String, ProviderHealth>, provider_id: &str) -> bool {
        health.get(provider_id).is_some_and(|health| {
            health
                .cooldown_until
                .is_some_and(|until| until > Instant::now())
        })
    }

    /// Resets a provider's breaker state after a successful attempt.
    fn report_success(&self, provider_id: &str) {
        if let Ok(mut queue) = self.queue.lock() {
            let health = queue.health.entry(provider_id.to_owned()).or_default();
            if health.consecutive_transport_failures > 0 || health.cooldown_until.is_some() {
                *health = ProviderHealth::default();
            }
        }
        // Slot release notifies the queue; no extra wake needed here.
    }

    /// Counts one transport-class failure; trips the breaker once the
    /// consecutive count reaches the threshold, doubling the cooldown per trip.
    fn report_transport_failure(&self, provider_id: &str) {
        if let Ok(mut queue) = self.queue.lock() {
            let health = queue.health.entry(provider_id.to_owned()).or_default();
            health.consecutive_transport_failures += 1;
            let now = Instant::now();
            let tripped = health.cooldown_until.is_none_or(|until| until <= now);
            if health.consecutive_transport_failures >= BREAKER_THRESHOLD && tripped {
                health.cooldown_until = Some(now + health.cooldown);
                tracing::warn!(
                    target: "app-core::project_execution",
                    provider_id = %provider_id,
                    failures = health.consecutive_transport_failures,
                    cooldown = ?health.cooldown,
                    "provider transport failures tripped the circuit breaker; cooling down"
                );
                health.cooldown = (health.cooldown * 2).min(BREAKER_COOLDOWN_MAX);
            }
        }
    }

    fn release(&self, provider_id: &str) {
        if let Ok(mut queue) = self.queue.lock() {
            if let Some(active) = queue.active.get_mut(provider_id) {
                *active = active.saturating_sub(1);
            }
            self.changed.notify_all();
        }
    }

    #[cfg(test)]
    fn provider_health(&self, provider_id: &str) -> Option<ProviderHealth> {
        self.queue.lock().ok()?.health.get(provider_id).cloned()
    }

    fn attempt(
        &self,
        prepared: &PreparedArtifactExecution,
        control: InvocationControl,
        structured: bool,
    ) -> Result<Value, AttemptFailure> {
        let response = self
            .llms
            .complete(
                &prepared.provider_id,
                prepared.request(structured),
                LlmExecutionControl::new(control),
            )
            .map_err(|failure| {
                if structured
                    && matches!(
                        failure.code,
                        LlmFailureCode::ProviderRejected
                            | LlmFailureCode::MalformedProviderResponse
                    )
                {
                    AttemptFailure::StructuredRejected(failure.to_string())
                } else if transient(&failure) {
                    AttemptFailure::Transient {
                        error: failure.to_string(),
                        transport: matches!(
                            failure.code,
                            LlmFailureCode::TransportReset | LlmFailureCode::DeadlineExceeded
                        ),
                    }
                } else {
                    AttemptFailure::Fatal(failure.to_string())
                }
            })?;
        // A reply that misses the contract is model nondeterminism: retry.
        prepared
            .task
            .parse_response(&response.content)
            .map_err(|error| AttemptFailure::Transient {
                error,
                transport: false,
            })
    }
}

fn transient(failure: &LlmFailure) -> bool {
    failure.retryable()
        || matches!(
            failure.code,
            LlmFailureCode::ProviderBusy | LlmFailureCode::MalformedProviderResponse
        )
}

impl PreparedArtifactExecution {
    fn request(&self, structured: bool) -> LlmRequest {
        LlmRequest {
            options: LlmRequestOptions {
                // No fixed token cap or temperature: the 60 s per-attempt
                // deadline already bounds runaway output, while a fixed cap
                // truncates reasoning models before they emit the JSON and
                // some models reject non-default temperatures outright.
                max_output_tokens: None,
                temperature: None,
                reasoning_effort: None,
                response_format: structured.then(|| LlmResponseFormat::JsonSchema {
                    name: "functional_artifact".into(),
                    schema: self.task.response_schema(),
                    strict: true,
                }),
            },
            messages: vec![LlmMessage {
                role: "user".into(),
                content: self.task.prompt(),
            }],
            stream: false,
            provider_id: Some(self.provider_id.clone()),
            model: self.model.clone(),
            conversation_id: None,
            provider_session_id: None,
            mcp_servers: vec![],
            modality_inputs: vec![],
        }
    }
}

#[cfg(test)]
mod tests;
