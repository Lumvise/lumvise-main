//! Provider-local bounded execution scheduler shared by desktop and server.

use std::{
    collections::HashMap,
    fmt,
    io::ErrorKind,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    time::Duration,
};

use super::{
    LlmRequest, LlmResponse, LlmSessionRoute,
    contract::{LlmProvider, ProviderCallControl},
    registry::LlmProviderRegistry,
};
use crate::NeuralError;
use lumvise_resource_routing::InvocationControl;

pub const PROVIDER_QUEUE_CAPACITY: usize = 8;
const EXECUTOR_POLL: Duration = Duration::from_millis(10);

/// Thin provider-call adapter over the one routing invocation control.
#[derive(Clone)]
pub struct LlmExecutionControl(InvocationControl, Option<Arc<dyn Fn() + Send + Sync>>);

impl fmt::Debug for LlmExecutionControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LlmExecutionControl")
            .field(&self.0)
            .finish()
    }
}

impl LlmExecutionControl {
    pub fn new(control: InvocationControl) -> Self {
        Self(control, None)
    }
    pub fn invocation(&self) -> &InvocationControl {
        &self.0
    }

    /// Observes actual provider entry without changing queue ownership.
    /// Example: `control.on_start(Arc::new(move || activity.mark_running()))`.
    pub fn on_start(mut self, callback: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.1 = Some(callback);
        self
    }

    fn started(&self) {
        if let Some(callback) = &self.1 {
            callback();
        }
    }
}

impl ProviderCallControl for LlmExecutionControl {
    fn remaining(&self) -> Duration {
        self.0.remaining()
    }
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    fn is_expired(&self) -> bool {
        self.0.is_expired()
    }
    fn invocation_control(&self) -> Option<&InvocationControl> {
        Some(&self.0)
    }
}

impl ProviderCallControl for InvocationControl {
    fn remaining(&self) -> Duration {
        self.remaining()
    }
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn is_expired(&self) -> bool {
        self.is_expired()
    }
    fn invocation_control(&self) -> Option<&InvocationControl> {
        Some(self)
    }
}

/// Whether a provider failure ends only the current turn or the entire session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFailureTier {
    Turn,
    Session,
}

/// Stable, machine-readable provider failure classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFailureCode {
    DeadlineExceeded,
    TransportReset,
    ProcessCrashed,
    ToolRoundsExhausted,
    MalformedProviderResponse,
    ProviderRejected,
    ProviderBusy,
    ProviderUnconfigured,
    AuthenticationRequired,
    Cancelled,
    AppInternal,
}

/// Typed provider failure retained across the executor, host, and plugin seams.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LlmFailure {
    pub tier: LlmFailureTier,
    pub code: LlmFailureCode,
    pub message: String,
}

impl LlmFailure {
    pub fn new(code: LlmFailureCode, message: impl Into<String>) -> Self {
        let tier = match code {
            LlmFailureCode::ProviderUnconfigured
            | LlmFailureCode::AuthenticationRequired
            | LlmFailureCode::Cancelled
            | LlmFailureCode::AppInternal => LlmFailureTier::Session,
            LlmFailureCode::DeadlineExceeded
            | LlmFailureCode::TransportReset
            | LlmFailureCode::ProcessCrashed
            | LlmFailureCode::ToolRoundsExhausted
            | LlmFailureCode::MalformedProviderResponse
            | LlmFailureCode::ProviderRejected
            | LlmFailureCode::ProviderBusy => LlmFailureTier::Turn,
        };
        Self {
            tier,
            code,
            message: message.into(),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self.code,
            LlmFailureCode::DeadlineExceeded | LlmFailureCode::TransportReset
        )
    }

    fn from_neural_error(error: NeuralError) -> Self {
        let message = error.to_string();
        let code = match error {
            NeuralError::InvalidValue { .. } | NeuralError::DbCore(_) => {
                LlmFailureCode::AppInternal
            }
            NeuralError::MissingValue { .. } => LlmFailureCode::ProviderUnconfigured,
            NeuralError::ProcessFailed { .. } => LlmFailureCode::ProcessCrashed,
            NeuralError::ProcessTimeout { .. } => LlmFailureCode::DeadlineExceeded,
            NeuralError::ProcessCancelled { .. } => LlmFailureCode::Cancelled,
            NeuralError::MalformedPayload { .. } | NeuralError::Json { .. } => {
                LlmFailureCode::MalformedProviderResponse
            }
            NeuralError::ProviderFailed { .. } => LlmFailureCode::ProviderRejected,
            NeuralError::ToolRoundsExhausted { .. } => LlmFailureCode::ToolRoundsExhausted,
            NeuralError::Io { source, .. } => match source.kind() {
                ErrorKind::NotFound | ErrorKind::PermissionDenied => {
                    LlmFailureCode::ProviderUnconfigured
                }
                ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::UnexpectedEof => {
                    LlmFailureCode::TransportReset
                }
                _ => LlmFailureCode::ProcessCrashed,
            },
            NeuralError::WarmingUp { .. } => LlmFailureCode::ProviderBusy,
        };
        Self::new(code, message)
    }
}

impl fmt::Display for LlmFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub struct LlmExecutorRegistry {
    providers: Arc<Mutex<LlmProviderRegistry>>,
    executors: Mutex<HashMap<String, Arc<ProviderExecutor>>>,
}

struct ProviderExecutor {
    provider: Arc<dyn LlmProvider>,
    senders: Vec<SyncSender<LlmJob>>,
    next_sender: AtomicUsize,
}

struct LlmJob {
    request: LlmRequest,
    control: LlmExecutionControl,
    response: SyncSender<crate::Result<LlmResponse>>,
}

impl LlmExecutorRegistry {
    pub fn new(providers: Arc<Mutex<LlmProviderRegistry>>) -> Arc<Self> {
        Arc::new(Self {
            providers,
            executors: Mutex::new(HashMap::new()),
        })
    }

    pub fn complete(
        &self,
        provider_id: &str,
        request: LlmRequest,
        control: LlmExecutionControl,
    ) -> Result<LlmResponse, LlmFailure> {
        self.executor(provider_id)?
            .complete(provider_id, request, control)
    }

    /// Parallel completions the provider's executor admits; `1` when the
    /// registry is unavailable. Callers size their own admission to it so
    /// queued work never waits inside the provider lane on a running deadline.
    pub fn completion_concurrency(&self, provider_id: &str) -> usize {
        self.providers
            .lock()
            .map(|providers| providers.completion_concurrency(provider_id).max(1))
            .unwrap_or(1)
    }

    pub fn resolve_session_route(
        &self,
        provider_id: &str,
        request: &LlmRequest,
    ) -> Result<LlmSessionRoute, LlmFailure> {
        let providers = self.providers.lock().map_err(|_| {
            LlmFailure::new(
                LlmFailureCode::AppInternal,
                "LLM provider registry mutex poisoned",
            )
        })?;
        providers
            .resolve_session_route(provider_id, request)
            .map_err(|error| {
                LlmFailure::new(LlmFailureCode::ProviderUnconfigured, error.to_string())
            })
    }
    fn executor(&self, provider_id: &str) -> Result<Arc<ProviderExecutor>, LlmFailure> {
        let (provider, completion_concurrency) = {
            let providers = self.providers.lock().map_err(|_| {
                LlmFailure::new(
                    LlmFailureCode::AppInternal,
                    "LLM provider registry mutex poisoned",
                )
            })?;
            let provider = providers.provider_handle(provider_id).map_err(|error| {
                LlmFailure::new(LlmFailureCode::ProviderUnconfigured, error.to_string())
            })?;
            (provider, providers.completion_concurrency(provider_id))
        };
        let mut executors = self.executors.lock().map_err(|_| {
            LlmFailure::new(
                LlmFailureCode::AppInternal,
                "LLM executor registry mutex poisoned",
            )
        })?;
        if let Some(executor) = executors.get(provider_id)
            && Arc::ptr_eq(&executor.provider, &provider)
        {
            return Ok(Arc::clone(executor));
        }
        let executor = Arc::new(ProviderExecutor::start(provider, completion_concurrency));
        executors.insert(provider_id.into(), Arc::clone(&executor));
        Ok(executor)
    }
}

impl ProviderExecutor {
    fn start(provider: Arc<dyn LlmProvider>, completion_concurrency: usize) -> Self {
        let mut senders = Vec::with_capacity(completion_concurrency.max(1));
        for _ in 0..completion_concurrency.max(1) {
            let (sender, receiver) = mpsc::sync_channel::<LlmJob>(PROVIDER_QUEUE_CAPACITY);
            let worker_provider = Arc::clone(&provider);
            std::thread::spawn(move || {
                while let Ok(job) = receiver.recv() {
                    if terminal_error(&job.control).is_some() {
                        continue;
                    }
                    job.control.started();
                    let result = worker_provider.complete_controlled(&job.request, &job.control);
                    // The bounded queue already serializes this worker. New jobs
                    // can wait here while a cancelled call finishes cleanup;
                    // no provider-wide isolation flag is needed or safe to race.
                    if terminal_error(&job.control).is_none() {
                        let _ = job.response.send(result);
                    }
                    // On terminal_error the caller already gave up and stopped
                    // receiving, so the result is silently dropped instead of
                    // permanently poisoning the provider.
                }
            });
            senders.push(sender);
        }
        Self {
            provider,
            senders,
            next_sender: AtomicUsize::new(0),
        }
    }

    fn complete(
        &self,
        provider_id: &str,
        request: LlmRequest,
        control: LlmExecutionControl,
    ) -> Result<LlmResponse, LlmFailure> {
        if let Some(error) = terminal_error(&control) {
            return Err(error);
        }
        let (response, receiver) = mpsc::sync_channel(1);
        let index = self.next_sender.fetch_add(1, Ordering::AcqRel) % self.senders.len();
        self.senders[index]
            .try_send(LlmJob {
                request,
                control: control.clone(),
                response,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => LlmFailure::new(
                    LlmFailureCode::ProviderBusy,
                    format!("LLM provider `{provider_id}` queue is full"),
                ),
                TrySendError::Disconnected(_) => LlmFailure::new(
                    LlmFailureCode::TransportReset,
                    format!("LLM provider `{provider_id}` worker disconnected"),
                ),
            })?;
        loop {
            if let Some(error) = terminal_error(&control) {
                return Err(error);
            }
            match receiver.recv_timeout(EXECUTOR_POLL) {
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(error)) => return Err(LlmFailure::from_neural_error(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(terminal_error(&control).unwrap_or_else(|| {
                        LlmFailure::new(
                            LlmFailureCode::TransportReset,
                            format!("LLM provider `{provider_id}` worker disconnected"),
                        )
                    }));
                }
            }
        }
    }
}

fn terminal_error(control: &LlmExecutionControl) -> Option<LlmFailure> {
    if control.0.is_cancelled() {
        Some(LlmFailure::new(
            LlmFailureCode::Cancelled,
            "LLM provider invocation was cancelled",
        ))
    } else if control.0.is_expired() {
        Some(LlmFailure::new(
            LlmFailureCode::DeadlineExceeded,
            "LLM provider invocation deadline elapsed",
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{NeuralError, Result};
    use crate::llm_providers::contract::LlmStreamEventSink;
    use crate::llm_providers::{
        LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities, LlmStreamEvent,
    };
    use crate::process::StreamControl;
    use serde_json::Value;
    use std::sync::mpsc;

    /// A fake provider whose first `complete_controlled` call blocks until the
    /// test releases it via a channel. This lets a caller deadline expire while
    /// the worker is provably mid-call — the exact scenario that used to
    /// permanently poison `isolated`. Subsequent calls return immediately.
    struct BlockingFakeProvider {
        id: String,
        release: parking_lot::Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl BlockingFakeProvider {
        fn new(id: &str, release: mpsc::Receiver<()>) -> Self {
            Self {
                id: id.to_string(),
                release: parking_lot::Mutex::new(Some(release)),
            }
        }
    }

    impl LlmProvider for BlockingFakeProvider {
        fn provider_id(&self) -> &str {
            &self.id
        }

        fn capabilities(&self) -> LlmProviderCapabilities {
            LlmProviderCapabilities {
                provider_id: self.id.clone(),
                final_text_output: LlmCapabilitySupport::Supported,
                streamed_text_output: LlmCapabilitySupport::Unsupported,
                image_snapshot_input: LlmCapabilitySupport::Unsupported,
                live_audio_input: LlmCapabilitySupport::Unsupported,
                screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
                native_audio_output: LlmCapabilitySupport::Unsupported,
            }
        }

        fn complete(&self, _request: &LlmRequest) -> Result<LlmResponse> {
            unreachable!("the executor dispatches via complete_controlled")
        }

        fn complete_controlled(
            &self,
            _request: &LlmRequest,
            _control: &dyn ProviderCallControl,
        ) -> Result<LlmResponse> {
            // First call: take the one-shot gate and block until the test
            // releases us, simulating a long in-flight call whose control
            // expires before it returns.
            let release = self.release.lock().take();
            if let Some(rx) = release {
                let _ = rx.recv();
                return Err(NeuralError::ProviderFailed {
                    provider_id: self.id.clone(),
                    message: "blocking call released".into(),
                });
            }
            Ok(LlmResponse {
                provider_id: self.id.clone(),
                model: "test-model".into(),
                content: "ok".into(),
                metadata: Value::Null,
            })
        }

        fn stream_with_events(
            &self,
            _request: &LlmRequest,
            _control: StreamControl,
            _on_event: &mut LlmStreamEventSink<'_>,
        ) -> Result<()> {
            unreachable!("streaming is not under test")
        }
    }

    fn test_request() -> LlmRequest {
        LlmRequest {
            options: Default::default(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            stream: false,
            provider_id: Some("test".into()),
            model: None,
            conversation_id: None,
            provider_session_id: None,
            mcp_servers: Vec::new(),
            modality_inputs: Vec::new(),
        }
    }

    #[test]
    fn expired_worker_allows_next_request_to_queue_while_settling() {
        let (release_tx, release_rx) = mpsc::channel();
        let registry = LlmProviderRegistry::from_provider_instances(vec![Box::new(
            BlockingFakeProvider::new("test", release_rx),
        )])
        .unwrap();
        let executor = LlmExecutorRegistry::new(Arc::new(Mutex::new(registry)));
        let short =
            LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_millis(50)));
        assert_eq!(
            executor
                .complete("test", test_request(), short)
                .unwrap_err()
                .code,
            LlmFailureCode::DeadlineExceeded
        );
        let (result_tx, result_rx) = mpsc::channel();
        let next = std::thread::spawn(move || {
            let control =
                LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_secs(5)));
            result_tx
                .send(executor.complete("test", test_request(), control))
                .unwrap();
        });
        let early = result_rx.recv_timeout(Duration::from_millis(100));
        release_tx.send(()).unwrap();
        next.join().unwrap();
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "request must queue, not fail while settling: {early:?}"
        );
        assert_eq!(result_rx.recv().unwrap().unwrap().content, "ok");
    }

    /// Regression: a call whose control expires mid-flight must NOT permanently
    /// disable the provider. Before the fix, `isolated` was set to `true` when
    /// the caller gave up and never cleared, so every subsequent call returned
    /// `Unavailable` for the rest of the process lifetime.
    #[test]
    fn expired_call_does_not_permanently_isolate_provider() {
        let (release_tx, release_rx) = mpsc::channel();
        let provider = BlockingFakeProvider::new("test", release_rx);
        let registry = LlmProviderRegistry::from_provider_instances(vec![Box::new(provider)])
            .expect("registry");
        let executor = LlmExecutorRegistry::new(Arc::new(Mutex::new(registry)));

        // Dispatch with a deadline too short for the blocking fake provider to
        // return on its own. The caller gives up while the worker is still
        // mid-call, which previously set the provider-wide isolation flag.
        let short =
            LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_millis(50)));
        let first = executor.complete("test", test_request(), short);
        assert!(
            matches!(
                first,
                Err(LlmFailure {
                    tier: LlmFailureTier::Turn,
                    code: LlmFailureCode::DeadlineExceeded,
                    ..
                })
            ),
            "first call should hit the deadline, got {first:?}"
        );

        // Release after the caller gives up: cleanup must not prevent a later
        // request from using the same worker.
        let _ = release_tx.send(());
        // Give the worker a moment to process the release and return to recv().
        std::thread::sleep(Duration::from_millis(100));

        // A fresh call with a generous deadline must succeed — the provider is
        // no longer poisoned.

        let long =
            LlmExecutionControl::new(InvocationControl::with_deadline(Duration::from_secs(5)));
        let second = executor.complete("test", test_request(), long);
        assert!(
            second.is_ok(),
            "provider should be available after cancelled call settled, got {second:?}"
        );
    }

    #[test]
    fn subprocess_exit_and_timeout_keep_typed_turn_failure_semantics() {
        let crash = LlmFailure::from_neural_error(NeuralError::ProcessFailed {
            command: "fixture-cli".into(),
            status: "exit status: 7".into(),
            diagnostics: crate::process::ProcessFailureDiagnostics::from_stderr("tool crashed"),
        });
        assert_eq!(crash.tier, LlmFailureTier::Turn);
        assert_eq!(crash.code, LlmFailureCode::ProcessCrashed);
        assert!(!crash.retryable());

        let timeout = LlmFailure::from_neural_error(NeuralError::ProcessTimeout {
            command: "fixture-cli".into(),
            timeout_ms: 180_000,
        });
        assert_eq!(timeout.tier, LlmFailureTier::Turn);
        assert_eq!(timeout.code, LlmFailureCode::DeadlineExceeded);
        assert!(timeout.retryable());
    }
}
