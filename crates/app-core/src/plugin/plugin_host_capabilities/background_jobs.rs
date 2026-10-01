//! Bounded asynchronous Host Capability jobs.

use lumvise_frontend_core::FrontendCore;
use lumvise_neural_core::llm_providers::{
    LlmExecutionControl, LlmExecutorRegistry, LlmFailure, LlmFailureCode, LlmSessionRoute,
};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use super::{
    BACKGROUND_JOB, NeutralLlmRequest, apply_configured_llm_selection, decode, exact_fields,
    exact_object, failed, invalid, non_empty_option, quota, required, required_string,
    selected_llm_executor,
};

const MAX_ACTIVE_JOBS: usize = 256;
const MAX_WAIT_MS: u64 = 55_000;
// Background jobs are explicitly decoupled from the foreground 60-second
// plugin-invocation policy, so they must not reuse
// `InvocationControl::sixty_seconds()`. Legitimate agentic CLI-provider turns
// (codex/claude/gemini, each configured with a 180s subprocess timeout)
// regularly exceed 60s; 5 minutes safely covers that ceiling.
const BACKGROUND_JOB_DEADLINE: Duration = Duration::from_secs(300);

pub(super) struct BackgroundJobAdapter {
    jobs: Arc<Mutex<HashMap<String, BackgroundJob>>>,
    finished: Arc<Condvar>,
    llm_executors: Arc<LlmExecutorRegistry>,
    assistant_llm_executors: Arc<LlmExecutorRegistry>,
    activity: Arc<crate::workspace_activity::WorkspaceActivity>,
    frontend: Arc<Mutex<FrontendCore>>,
    session_routes: Arc<Mutex<HashMap<String, LlmSessionRoute>>>,
    sequence: AtomicU64,
}

#[derive(Clone)]
struct BackgroundJob {
    plugin_id: String,
    job_id: String,
    job_kind: String,
    status: String,
    message: Option<String>,
    failure: Option<LlmFailure>,
    response: Option<Value>,
    control: LlmExecutionControl,
    route: Option<LlmSessionRoute>,
}

impl BackgroundJobAdapter {
    pub(super) fn new(
        llm_executors: Arc<LlmExecutorRegistry>,
        assistant_llm_executors: Arc<LlmExecutorRegistry>,
        frontend: Arc<Mutex<FrontendCore>>,
        activity: Arc<crate::workspace_activity::WorkspaceActivity>,
    ) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            finished: Arc::new(Condvar::new()),
            llm_executors,
            assistant_llm_executors,
            activity,
            frontend,
            session_routes: Arc::new(Mutex::new(HashMap::new())),
            sequence: AtomicU64::new(1),
        }
    }

    pub(super) fn invoke(
        &self,
        plugin_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        let fields = exact_object(BACKGROUND_JOB, &input)?;
        match required_string(BACKGROUND_JOB, fields, "operation")? {
            "accept_llm" => self.accept(plugin_id, fields),
            "status" => self.status(plugin_id, fields),
            "wait" => self.wait(plugin_id, fields),
            "cancel" => self.cancel(plugin_id, fields),
            other => Err(invalid(
                BACKGROUND_JOB,
                &json!(other),
                "operation accept_llm, status, wait, or cancel",
            )),
        }
    }

    fn accept(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(
            BACKGROUND_JOB,
            fields,
            &["operation", "job_kind", "request"],
        )?;
        let kind = required_string(BACKGROUND_JOB, fields, "job_kind")?.to_owned();
        let input = required(fields, "request")?.clone();
        let mut request: NeutralLlmRequest = decode(BACKGROUND_JOB, input)?;
        request.validate()?;
        apply_configured_llm_selection(&self.frontend, BACKGROUND_JOB, &mut request)?;
        let llm_executors = selected_llm_executor(
            plugin_id,
            &self.llm_executors,
            &self.assistant_llm_executors,
        );
        let route = self.resolve_route(&request, llm_executors)?;
        let control =
            LlmExecutionControl::new(InvocationControl::with_deadline(BACKGROUND_JOB_DEADLINE));
        let job_id = self.reserve(plugin_id, kind, control.clone(), route.clone())?;
        let ticket = self.activity.track_llm(
            plugin_id,
            request.provider_id.as_deref().unwrap_or("unconfigured"),
        );
        spawn_job(
            Arc::downgrade(&self.jobs),
            Arc::downgrade(&self.finished),
            Arc::downgrade(llm_executors),
            job_id.clone(),
            request,
            control,
            ticket,
        );
        Ok(json!({"job_id": job_id}))
    }
    fn resolve_route(
        &self,
        request: &NeutralLlmRequest,
        llm_executors: &LlmExecutorRegistry,
    ) -> Result<Option<LlmSessionRoute>, HostCapabilityError> {
        let Some(provider_id) = non_empty_option(request.provider_id.as_deref()) else {
            return Ok(None);
        };
        let Some(session_id) = request
            .conversation_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        else {
            let result =
                llm_executors.resolve_session_route(provider_id, &request.clone().into_neural());
            return match result {
                Ok(route) => Ok(Some(route)),
                Err(failure) if failure.code == LlmFailureCode::ProviderUnconfigured => Ok(None),
                Err(failure) => Err(failed(BACKGROUND_JOB, &failure.message)),
            };
        };
        let mut routes = self
            .session_routes
            .lock()
            .map_err(|_| failed(BACKGROUND_JOB, "assistant session route mutex poisoned"))?;
        if let Some(route) = routes.get(session_id) {
            if route.provider_id() != provider_id {
                return Err(invalid(
                    BACKGROUND_JOB,
                    &json!(provider_id),
                    "the assistant session provider cannot change after route selection",
                ));
            }
            return Ok(Some(route.clone()));
        }
        let route =
            llm_executors.resolve_session_route(provider_id, &request.clone().into_neural());
        let route = match route {
            Ok(route) => route,
            Err(failure) if failure.code == LlmFailureCode::ProviderUnconfigured => {
                return Ok(None);
            }
            Err(failure) => return Err(failed(BACKGROUND_JOB, &failure.message)),
        };
        routes.insert(session_id.to_owned(), route.clone());
        Ok(Some(route))
    }

    fn reserve(
        &self,
        plugin_id: &str,
        job_kind: String,
        control: LlmExecutionControl,
        route: Option<LlmSessionRoute>,
    ) -> Result<String, HostCapabilityError> {
        let mut jobs = self.lock_jobs()?;
        if jobs.len() >= MAX_ACTIVE_JOBS {
            return Err(quota(BACKGROUND_JOB, jobs.len() + 1, MAX_ACTIVE_JOBS));
        }
        let job_id = format!(
            "compiled-job-{}",
            self.sequence.fetch_add(1, Ordering::Relaxed)
        );
        jobs.insert(
            job_id.clone(),
            BackgroundJob::pending(plugin_id, &job_id, job_kind, control, route),
        );
        Ok(job_id)
    }

    fn status(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(BACKGROUND_JOB, fields, &["operation", "job_id"])?;
        let job_id = required_string(BACKGROUND_JOB, fields, "job_id")?;
        let mut jobs = self.lock_jobs()?;
        let Some(job) = jobs
            .get(job_id)
            .filter(|job| job.plugin_id == plugin_id)
            .cloned()
        else {
            return Ok(json!({"job": null}));
        };
        if job.status != "pending" {
            jobs.remove(job_id);
        }
        Ok(job.output())
    }

    fn wait(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(
            BACKGROUND_JOB,
            fields,
            &["operation", "job_id", "timeout_ms"],
        )?;
        let job_id = required_string(BACKGROUND_JOB, fields, "job_id")?;
        let timeout_ms = required_wait_ms(fields)?;
        let jobs = self.lock_jobs()?;
        let (mut jobs, _) = self
            .finished
            .wait_timeout_while(jobs, Duration::from_millis(timeout_ms), |jobs| {
                job_is_pending(jobs, plugin_id, job_id)
            })
            .map_err(|_| failed(BACKGROUND_JOB, "background job wait mutex poisoned"))?;
        Ok(take_waited_job(&mut jobs, plugin_id, job_id, timeout_ms))
    }

    fn cancel(
        &self,
        plugin_id: &str,
        fields: &Map<String, Value>,
    ) -> Result<Value, HostCapabilityError> {
        exact_fields(BACKGROUND_JOB, fields, &["operation", "job_id"])?;
        let job_id = required_string(BACKGROUND_JOB, fields, "job_id")?;
        let mut jobs = self.lock_jobs()?;
        let Some(mut job) = jobs.remove(job_id).filter(|job| job.plugin_id == plugin_id) else {
            return Ok(json!({"job": null}));
        };
        job.control.invocation().cancel();
        job.status = "cancelled".into();
        job.message = Some("cancelled by owning plugin".into());
        self.finished.notify_all();
        Ok(job.output())
    }

    fn lock_jobs(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, BackgroundJob>>, HostCapabilityError>
    {
        self.jobs
            .lock()
            .map_err(|_| failed(BACKGROUND_JOB, "background job mutex poisoned"))
    }
}

fn required_wait_ms(fields: &Map<String, Value>) -> Result<u64, HostCapabilityError> {
    let value = required(fields, "timeout_ms")?;
    value
        .as_u64()
        .filter(|timeout| (1..=MAX_WAIT_MS).contains(timeout))
        .ok_or_else(|| {
            invalid(
                BACKGROUND_JOB,
                value,
                "timeout_ms integer from 1 through 55000",
            )
        })
}

fn job_is_pending(jobs: &HashMap<String, BackgroundJob>, plugin_id: &str, job_id: &str) -> bool {
    jobs.get(job_id)
        .is_some_and(|job| job.plugin_id == plugin_id && job.status == "pending")
}

fn take_waited_job(
    jobs: &mut HashMap<String, BackgroundJob>,
    plugin_id: &str,
    job_id: &str,
    timeout_ms: u64,
) -> Value {
    let Some(mut job) = jobs.remove(job_id).filter(|job| job.plugin_id == plugin_id) else {
        return json!({"job": null});
    };
    if job.status == "pending" {
        job.status = "failed".into();
        job.message = Some(format!(
            "background job `{job_id}` exceeded {timeout_ms} ms; expected terminal result"
        ));
    }
    job.output()
}

impl BackgroundJob {
    fn pending(
        plugin_id: &str,
        job_id: &str,
        job_kind: String,
        control: LlmExecutionControl,
        route: Option<LlmSessionRoute>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            job_id: job_id.into(),
            job_kind,
            status: "pending".into(),
            message: None,
            failure: None,
            response: None,
            control,
            route,
        }
    }

    fn output(self) -> Value {
        let failure = self.failure.as_ref().map(|failure| {
            json!({
                "tier": failure.tier,
                "code": failure.code,
                "message": failure.message,
                "retryable": failure.retryable(),
            })
        });
        json!({"job_id": self.job_id, "job_kind": self.job_kind,
        "status": self.status, "message": self.message, "failure": failure,
        "response": self.response,
        "route": self.route.as_ref().map(|route| json!({
            "provider_id": route.provider_id(),
            "transport": route.transport().metric_label(),
        }))})
    }
}

fn spawn_job(
    jobs: Weak<Mutex<HashMap<String, BackgroundJob>>>,
    finished: Weak<Condvar>,
    llm_executors: Weak<LlmExecutorRegistry>,
    job_id: String,
    request: NeutralLlmRequest,
    control: LlmExecutionControl,
    ticket: crate::workspace_activity::ActivityTicket,
) {
    std::thread::spawn(move || {
        let Some(llm_executors) = llm_executors.upgrade() else {
            return;
        };
        let result = complete(&llm_executors, request, ticket.control(control));
        ticket.finish(&result);
        let Some(jobs) = jobs.upgrade() else { return };
        let Some(finished) = finished.upgrade() else {
            return;
        };
        update_job(&jobs, &finished, &job_id, result);
    });
}

fn complete(
    llm_executors: &LlmExecutorRegistry,
    request: NeutralLlmRequest,
    control: LlmExecutionControl,
) -> Result<Value, LlmFailure> {
    let provider = non_empty_option(request.provider_id.as_deref())
        .ok_or_else(|| {
            LlmFailure::new(
                LlmFailureCode::ProviderUnconfigured,
                "no provider_id was selected",
            )
        })?
        .to_owned();
    let response = llm_executors.complete(&provider, request.into_neural(), control)?;
    serde_json::to_value(response).map_err(|error| {
        LlmFailure::new(
            LlmFailureCode::AppInternal,
            format!("could not serialize provider response: {error}"),
        )
    })
}

fn update_job(
    jobs: &Mutex<HashMap<String, BackgroundJob>>,
    finished: &Condvar,
    job_id: &str,
    result: Result<Value, LlmFailure>,
) {
    let Ok(mut jobs) = jobs.lock() else { return };
    let Some(job) = jobs.get_mut(job_id) else {
        return;
    };
    match result {
        Ok(response) => {
            job.status = "completed".into();
            job.response = Some(response);
        }
        Err(failure) => {
            job.status = "failed".into();
            job.message = Some(failure.message.clone());
            job.failure = Some(failure);
        }
    }
    finished.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumvise_neural_core::{
        LlmProviderRegistry, NeuralError,
        llm_providers::{
            LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities, LlmRequest, LlmResponse,
            contract::{LlmProvider, LlmStreamEventSink, ProviderCallControl},
        },
        process::StreamControl,
    };

    struct TimeoutProvider;

    impl LlmProvider for TimeoutProvider {
        fn provider_id(&self) -> &str {
            "timeout"
        }

        fn capabilities(&self) -> LlmProviderCapabilities {
            LlmProviderCapabilities {
                provider_id: "timeout".into(),
                final_text_output: LlmCapabilitySupport::Supported,
                streamed_text_output: LlmCapabilitySupport::Unsupported,
                image_snapshot_input: LlmCapabilitySupport::Unsupported,
                live_audio_input: LlmCapabilitySupport::Unsupported,
                screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
                native_audio_output: LlmCapabilitySupport::Unsupported,
            }
        }

        fn complete(&self, _request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
            Err(NeuralError::ProcessTimeout {
                command: "deterministic-timeout".into(),
                timeout_ms: 180_000,
            })
        }

        fn complete_controlled(
            &self,
            request: &LlmRequest,
            _control: &dyn ProviderCallControl,
        ) -> lumvise_neural_core::Result<LlmResponse> {
            self.complete(request)
        }

        fn stream_with_events(
            &self,
            _request: &LlmRequest,
            _control: StreamControl,
            _on_event: &mut LlmStreamEventSink<'_>,
        ) -> lumvise_neural_core::Result<()> {
            unreachable!("streaming is not under test")
        }
    }

    #[test]
    fn session_route_is_selected_once_and_rejects_provider_changes() {
        let registry =
            LlmProviderRegistry::from_provider_instances(vec![Box::new(TimeoutProvider)])
                .expect("timeout provider registry");
        let providers = Arc::new(Mutex::new(registry));
        let adapter = BackgroundJobAdapter::new(
            LlmExecutorRegistry::new(Arc::clone(&providers)),
            LlmExecutorRegistry::new(providers),
            Arc::new(Mutex::new(FrontendCore::default())),
            Arc::default(),
        );
        let request = NeutralLlmRequest {
            provider_id: Some("timeout".into()),
            model: None,
            conversation_id: Some("session-a".into()),
            llm_session_id: None,
            messages: vec![LlmMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
            mcp_servers: Vec::new(),
            options: Default::default(),
        };
        let route = adapter
            .resolve_route(&request, &adapter.llm_executors)
            .expect("resolve route")
            .expect("route");
        assert_eq!(route.transport().metric_label(), "direct_api");

        let changed = NeutralLlmRequest {
            provider_id: Some("other".into()),
            ..request
        };
        let error = adapter
            .resolve_route(&changed, &adapter.llm_executors)
            .expect_err("provider change rejected");
        assert!(error.to_string().contains("cannot change"));
    }

    #[test]
    fn accept_llm_falls_back_to_the_configured_assistant_engine_when_omitted() {
        let mut configured_settings = lumvise_frontend_core::AppSettings::default();
        configured_settings.assistant_engine = lumvise_frontend_core::AssistantEngine::Cerebras;
        let configured_frontend = Arc::new(Mutex::new(FrontendCore::new(configured_settings)));
        let base_request = NeutralLlmRequest {
            provider_id: None,
            model: None,
            conversation_id: None,
            llm_session_id: None,
            messages: vec![LlmMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
            mcp_servers: Vec::new(),
            options: Default::default(),
        };

        let mut omitted = base_request.clone();
        let resolved =
            apply_configured_llm_selection(&configured_frontend, BACKGROUND_JOB, &mut omitted)
                .expect("configured fallback resolves");
        assert_eq!(resolved.as_deref(), Some("cerebras"));
        assert_eq!(omitted.provider_id.as_deref(), Some("cerebras"));

        let mut explicit = NeutralLlmRequest {
            provider_id: Some("openrouter".into()),
            ..base_request.clone()
        };
        let resolved =
            apply_configured_llm_selection(&configured_frontend, BACKGROUND_JOB, &mut explicit)
                .expect("explicit provider is preserved over the configured engine");
        assert_eq!(resolved.as_deref(), Some("openrouter"));
        assert_eq!(explicit.provider_id.as_deref(), Some("openrouter"));

        let unconfigured_frontend = Arc::new(Mutex::new(FrontendCore::default()));
        let mut none_available = base_request;
        let resolved = apply_configured_llm_selection(
            &unconfigured_frontend,
            BACKGROUND_JOB,
            &mut none_available,
        )
        .expect("an unresolved provider defers to job completion instead of erroring");
        assert_eq!(resolved, None);
        assert_eq!(none_available.provider_id, None);
    }

    #[test]
    fn terminal_status_consumption_keeps_sequential_queue_usable() {
        let adapter = empty_adapter();
        for index in 0..=MAX_ACTIVE_JOBS {
            let accepted = adapter
                .invoke(
                    "plugin.test",
                    json!({"operation": "accept_llm", "job_kind": format!("job-{index}"),
                    "request": {"provider_id": null, "model": null,
                    "conversation_id": null, "llm_session_id": null,
                    "messages": [{"role": "user", "content": "bounded"}], "mcp_servers": []}}),
                )
                .expect("sequential job accepted");
            consume_terminal(&adapter, accepted["job_id"].as_str().expect("job id"));
        }
    }

    #[test]
    fn unknown_or_foreign_job_status_is_reported_as_missing() {
        let adapter = empty_adapter();
        let job_id = adapter
            .reserve(
                "plugin.owner",
                "owned".into(),
                LlmExecutionControl::new(InvocationControl::sixty_seconds()),
                None,
            )
            .expect("reserve job");

        let status = adapter
            .invoke(
                "plugin.foreign",
                json!({"operation": "status", "job_id": job_id}),
            )
            .expect("foreign status is indistinguishable from missing");

        assert_eq!(status, json!({"job": null}));
    }

    #[test]
    fn cancelled_job_rejects_late_completion_and_is_no_longer_pollable() {
        let adapter = empty_adapter();
        let job_id = adapter
            .reserve(
                "plugin.owner",
                "owned".into(),
                LlmExecutionControl::new(InvocationControl::sixty_seconds()),
                None,
            )
            .expect("reserve job");

        let cancelled = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "cancel", "job_id": job_id}),
            )
            .expect("cancel job");
        assert_eq!(cancelled["status"], "cancelled");
        update_job(
            &adapter.jobs,
            &adapter.finished,
            &job_id,
            Ok(json!({"content": "late"})),
        );
        let missing = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "status", "job_id": job_id}),
            )
            .expect("cancelled job status");
        assert_eq!(missing, json!({"job": null}));
    }

    #[test]
    fn wait_returns_and_consumes_terminal_job() {
        let adapter = empty_adapter();
        let job_id = adapter
            .reserve(
                "plugin.owner",
                "owned".into(),
                LlmExecutionControl::new(InvocationControl::sixty_seconds()),
                None,
            )
            .expect("reserve job");
        update_job(
            &adapter.jobs,
            &adapter.finished,
            &job_id,
            Ok(json!({"content": "done"})),
        );

        let output = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "wait", "job_id": job_id, "timeout_ms": 10}),
            )
            .expect("wait for terminal job");

        assert_eq!(output["status"], "completed");
    }

    #[test]
    fn failed_job_preserves_typed_provider_failure() {
        let adapter = empty_adapter();
        let job_id = adapter
            .reserve(
                "plugin.owner",
                "owned".into(),
                LlmExecutionControl::new(InvocationControl::sixty_seconds()),
                None,
            )
            .expect("reserve job");
        update_job(
            &adapter.jobs,
            &adapter.finished,
            &job_id,
            Err(LlmFailure::new(
                LlmFailureCode::DeadlineExceeded,
                "CLI subprocess timed out",
            )),
        );

        let output = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "status", "job_id": job_id}),
            )
            .expect("typed failure status");
        assert_eq!(output["status"], "failed");
        assert_eq!(output["message"], "CLI subprocess timed out");
        assert_eq!(output["failure"]["tier"], "turn");
        assert_eq!(output["failure"]["code"], "deadline_exceeded");
        assert_eq!(output["failure"]["retryable"], true);
    }

    #[test]
    fn executor_timeout_reaches_background_job_as_retryable_turn_failure() {
        let registry =
            LlmProviderRegistry::from_provider_instances(vec![Box::new(TimeoutProvider)])
                .expect("timeout provider registry");
        let providers = Arc::new(Mutex::new(registry));
        let adapter = BackgroundJobAdapter::new(
            LlmExecutorRegistry::new(Arc::clone(&providers)),
            LlmExecutorRegistry::new(providers),
            Arc::new(Mutex::new(FrontendCore::default())),
            Arc::default(),
        );
        let accepted = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "accept_llm", "job_kind": "timeout",
                    "request": {"provider_id": "timeout", "model": null,
                    "conversation_id": null, "llm_session_id": null,
                    "messages": [{"role": "user", "content": "timeout"}], "mcp_servers": []}}),
            )
            .expect("accept timeout job");
        let job_id = accepted["job_id"].as_str().expect("job id").to_string();
        for _ in 0..1_000 {
            let status = adapter
                .invoke(
                    "plugin.owner",
                    json!({"operation": "status", "job_id": job_id}),
                )
                .expect("timeout job status");
            if status["status"] != "pending" {
                assert_eq!(status["status"], "failed");
                assert_eq!(status["failure"]["tier"], "turn");
                assert_eq!(status["failure"]["code"], "deadline_exceeded");
                assert_eq!(status["failure"]["retryable"], true);
                return;
            }
            std::thread::yield_now();
        }
        panic!("timeout job did not terminate within bounded polling");
    }

    #[test]
    fn wait_converts_timeout_to_terminal_failure() {
        let adapter = empty_adapter();
        let job_id = adapter
            .reserve(
                "plugin.owner",
                "owned".into(),
                LlmExecutionControl::new(InvocationControl::sixty_seconds()),
                None,
            )
            .expect("reserve job");

        let output = adapter
            .invoke(
                "plugin.owner",
                json!({"operation": "wait", "job_id": job_id, "timeout_ms": 1}),
            )
            .expect("bounded wait");

        assert_eq!(output["status"], "failed");
    }

    fn consume_terminal(adapter: &BackgroundJobAdapter, job_id: &str) {
        for _ in 0..1_000 {
            let status = adapter
                .invoke(
                    "plugin.test",
                    json!({"operation": "status", "job_id": job_id}),
                )
                .expect("job status");
            if status["status"] != "pending" {
                let consumed = adapter
                    .invoke(
                        "plugin.test",
                        json!({"operation": "status", "job_id": job_id}),
                    )
                    .expect("consumed terminal job is missing");
                assert_eq!(consumed, json!({"job": null}));
                return;
            }
            std::thread::yield_now();
        }
        panic!("job `{job_id}` did not terminate within bounded polling");
    }

    fn empty_adapter() -> BackgroundJobAdapter {
        let providers = Arc::new(Mutex::new(LlmProviderRegistry::empty()));
        BackgroundJobAdapter::new(
            LlmExecutorRegistry::new(Arc::clone(&providers)),
            LlmExecutorRegistry::new(providers),
            Arc::new(Mutex::new(FrontendCore::default())),
            Arc::default(),
        )
    }
}
