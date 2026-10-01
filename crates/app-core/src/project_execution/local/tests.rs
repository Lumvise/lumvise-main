use super::*;
use lumvise_frontend_core::AppSettings;
use lumvise_neural_core::LlmProviderRegistry;
use lumvise_neural_core::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use lumvise_neural_core::llm_providers::{
    LlmCapabilitySupport, LlmProviderCapabilities, LlmResponse,
};
use lumvise_neural_core::process::StreamControl;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

struct FakeArtifactEngine {
    provider_id: &'static str,
    requests: mpsc::Sender<LlmRequest>,
    response: parking_lot::Mutex<mpsc::Receiver<String>>,
    reject_structured: bool,
    /// Remaining `complete` calls that fail with a transport-class reset.
    fail_transport_first: Arc<AtomicUsize>,
}
impl LlmProvider for FakeArtifactEngine {
    fn provider_id(&self) -> &str {
        self.provider_id
    }
    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: "cerebras".into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Unsupported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }
    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        if self.fail_transport_first.load(Ordering::Relaxed) > 0 {
            self.fail_transport_first.fetch_sub(1, Ordering::Relaxed);
            return Err(lumvise_neural_core::NeuralError::Io {
                value: "cerebras endpoint".into(),
                expected: "reachable LLM provider endpoint".into(),
                source: std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset"),
            });
        }
        self.requests.send(request.clone()).unwrap();
        if self.reject_structured && request.options.response_format.is_some() {
            return Err(lumvise_neural_core::NeuralError::ProviderFailed {
                provider_id: "cerebras".into(),
                message: "provider rejected response_format json_schema".into(),
            });
        }
        let content = self
            .response
            .lock()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(LlmResponse {
            provider_id: "cerebras".into(),
            model: request.model.clone().unwrap(),
            content,
            metadata: json!({}),
        })
    }
    fn stream_with_events(
        &self,
        _: &LlmRequest,
        _: StreamControl,
        _: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        unreachable!("task uses final text")
    }
}

struct LocalArtifactFixture {
    root: tempfile::TempDir,
    service: ProjectExecutionService,
    host: Arc<crate::plugin::PluginHostServices>,
    requests: mpsc::Receiver<LlmRequest>,
    reply: mpsc::Sender<String>,
    fail_transport_first: Arc<AtomicUsize>,
}
impl LocalArtifactFixture {
    fn new() -> Self {
        Self::with_concurrency(1)
    }
    fn with_concurrency(concurrency: usize) -> Self {
        Self::with_settings(concurrency, Self::default_settings(), false)
    }
    fn default_settings() -> AppSettings {
        AppSettings {
            assistant_engine: AssistantEngine::Cerebras,
            assistant_model: Some("selected-model".into()),
            ..Default::default()
        }
    }
    fn with_settings(concurrency: usize, settings: AppSettings, reject_structured: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("parser.rs"), "fn parse() { return; }\n").unwrap();
        let (requests, received) = mpsc::channel();
        let (reply, response) = mpsc::channel();
        let fail_transport_first = Arc::new(AtomicUsize::new(0));
        // The fake serves the engine the executor will actually dispatch to.
        let provider_id = match settings
            .generation_engine
            .unwrap_or(settings.assistant_engine)
        {
            AssistantEngine::Codex => "codex",
            _ => "cerebras",
        };
        let providers =
            LlmProviderRegistry::from_provider_instances(vec![Box::new(FakeArtifactEngine {
                provider_id,
                requests,
                response: parking_lot::Mutex::new(response),
                reject_structured,
                fail_transport_first: Arc::clone(&fail_transport_first),
            })])
            .unwrap()
            .with_completion_concurrency(provider_id, concurrency);
        let host = crate::plugin::PluginHostServices::new(
            FrontendCore::new(settings),
            providers,
            Arc::new(lumvise_db_core::LocalPersistence::in_memory().expect("test persistence")),
        );
        let service = ProjectExecutionService::with_local_artifacts(
            host.frontend(),
            host.llm_executors(),
            host.activity(),
        );
        host.install_project_execution(service.clone());
        Self {
            root,
            service,
            host,
            requests: received,
            reply,
            fail_transport_first,
        }
    }
    fn fail_transport(&self, count: usize) {
        self.fail_transport_first.store(count, Ordering::Relaxed);
    }
    fn health(&self, provider_id: &str) -> ProviderHealth {
        let executor = self
            .service
            .lock_state()
            .unwrap()
            .local_executor
            .clone()
            .unwrap();
        executor.provider_health(provider_id).unwrap()
    }
    fn request(&self) -> ProjectExecutionRequest {
        ProjectExecutionRequest {
            requester_id: "builtin.knowledge".into(),
            project_root: self.root.path().to_str().unwrap().into(),
            capability_id: ARTIFACT_CAPABILITY.into(),
            idempotency_key: "functional:parse:abc".into(),
            input: json!({"semantic_element_id":"fn:parse", "element_kind":"function", "name":"parse", "path":"parser.rs", "start_line":1, "end_line":1, "content_fingerprint":"sha256:abc", "requested_artifact_kinds":["job","receives","outcome","effects"]}),
            priority: ProjectExecutionPriority::default(),
        }
    }
    fn output(&self) -> String {
        json!({"semantic_element_id":"fn:parse", "content_fingerprint":"sha256:abc", "job":"Parses input.", "source_interface":"parse()", "receives":[], "outcome":"Returns.", "effects":[]}).to_string()
    }
    fn wait_terminal(&self, job: &ProjectExecutionJob) -> ProjectExecutionJob {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let current = self.service.status(&job.requester_id, &job.job_id).unwrap();
            if !matches!(
                current.status,
                ProjectExecutionStatus::Queued | ProjectExecutionStatus::Running
            ) {
                return current;
            }
            assert!(
                Instant::now() < deadline,
                "job failed to terminate: {current:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn artifact_requests_constrain_structured_output() {
    let fixture = LocalArtifactFixture::new();
    let job = fixture.service.submit(fixture.request()).unwrap();
    let request = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(request.options.max_output_tokens, None);
    assert_eq!(request.options.temperature, None);
    assert_eq!(request.options.reasoning_effort, None);
    match &request.options.response_format {
        Some(LlmResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        }) => {
            assert_eq!(name, "functional_artifact");
            assert!(strict);
            assert_eq!(
                schema["properties"]["semantic_element_id"]["enum"],
                json!(["fn:parse"])
            );
            assert_eq!(
                schema["properties"]["content_fingerprint"]["enum"],
                json!(["sha256:abc"])
            );
        }
        other => panic!("expected json_schema response format: {other:?}"),
    }
    fixture.reply.send(fixture.output()).unwrap();
    let completed = fixture.wait_terminal(&job);
    assert_eq!(completed.status, ProjectExecutionStatus::Succeeded);
}

#[test]
fn no_mcp_uses_selected_engine_model_source_and_deduplicates() {
    let fixture = LocalArtifactFixture::new();
    let job = fixture.service.submit(fixture.request()).unwrap();
    let request = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(request.provider_id.as_deref(), Some("cerebras"));
    assert_eq!(request.model.as_deref(), Some("selected-model"));
    assert!(
        request.messages[0]
            .content
            .contains("fn parse() { return; }")
    );
    assert!(request.mcp_servers.is_empty());
    assert_eq!(
        fixture.service.submit(fixture.request()).unwrap().job_id,
        job.job_id
    );
    fixture.reply.send(fixture.output()).unwrap();
    let completed = fixture.wait_terminal(&job);
    assert_eq!(completed.status, ProjectExecutionStatus::Succeeded);
    assert_eq!(completed.output.unwrap()["semantic_element_id"], "fn:parse");
    assert!(
        fixture
            .service
            .status("another-requester", &job.job_id)
            .is_err()
    );
}

#[test]
fn connected_mcp_provider_remains_preferred() {
    let fixture = LocalArtifactFixture::new();
    let (sender, receiver) = mpsc::sync_channel(2);
    fixture
        .service
        .register_provider(
            "mcp",
            fixture.root.path().to_str().unwrap(),
            [ARTIFACT_CAPABILITY.into()],
            sender,
        )
        .unwrap();
    fixture.service.submit(fixture.request()).unwrap();
    assert!(matches!(
        receiver.recv().unwrap(),
        ProjectExecutionCommand::Execute { .. }
    ));
    assert!(fixture.requests.try_recv().is_err());
}

#[test]
fn local_job_is_not_redispatched_or_completed_by_new_mcp_connection() {
    let fixture = LocalArtifactFixture::new();
    let job = fixture.service.submit(fixture.request()).unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(2);
    fixture
        .service
        .register_provider(
            "mcp",
            fixture.root.path().to_str().unwrap(),
            [ARTIFACT_CAPABILITY.into()],
            sender,
        )
        .unwrap();
    assert!(receiver.try_recv().is_err());
    assert!(
        fixture
            .service
            .record_result("mcp", &job.job_id, Ok(Value::Null))
            .is_err()
    );
    fixture
        .service
        .expire_provider_leases_at(Instant::now() + Duration::from_secs(30))
        .unwrap();
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&job).status,
        ProjectExecutionStatus::Succeeded
    );
}

#[test]
fn cancellation_survives_late_local_result_and_bad_identity_fails() {
    let fixture = LocalArtifactFixture::new();
    let job = fixture.service.submit(fixture.request()).unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    fixture
        .service
        .cancel(&job.requester_id, &job.job_id)
        .unwrap();
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&job).status,
        ProjectExecutionStatus::Cancelled
    );
    let mut request = fixture.request();
    request.idempotency_key = "another".into();
    let next = fixture.service.submit(request).unwrap();
    // A reply for the wrong target is retried; it fails once every attempt did.
    for _ in 0..MAX_ATTEMPTS {
        fixture
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        fixture
            .reply
            .send(fixture.output().replace("sha256:abc", "wrong-target"))
            .unwrap();
    }
    let failed = fixture.wait_terminal(&next);
    assert_eq!(failed.status, ProjectExecutionStatus::Failed);
    let error = failed.error.unwrap();
    assert!(error.contains("expected `fn:parse` fingerprint `sha256:abc`"));
    assert!(error.contains(&format!("after {MAX_ATTEMPTS} attempts")));
}

#[test]
fn an_unparseable_reply_is_retried_and_the_next_valid_reply_succeeds() {
    let fixture = LocalArtifactFixture::new();
    let job = fixture.service.submit(fixture.request()).unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    fixture
        .reply
        .send("I could not follow the format.".into())
        .unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&job).status,
        ProjectExecutionStatus::Succeeded
    );
}

#[test]
fn a_concurrent_provider_runs_leaves_in_parallel() {
    // Two slots: both leaves reach the engine before either reply is sent.
    let fixture = LocalArtifactFixture::with_concurrency(2);
    let jobs: Vec<_> = (0..2)
        .map(|index| {
            let mut request = fixture.request();
            request.idempotency_key = format!("parallel-{index}");
            fixture.service.submit(request).unwrap()
        })
        .collect();
    for _ in &jobs {
        fixture
            .requests
            .recv_timeout(Duration::from_secs(5))
            .expect("both leaves dispatched without waiting for a reply");
    }
    for _ in &jobs {
        fixture.reply.send(fixture.output()).unwrap();
    }
    for job in jobs {
        assert_eq!(
            fixture.wait_terminal(&job).status,
            ProjectExecutionStatus::Succeeded
        );
    }
}

#[test]
fn nonlocal_projects_and_unknown_capabilities_never_spawn_engine() {
    let fixture = LocalArtifactFixture::new();
    let mut request = fixture.request();
    request.project_root = fixture.root.path().join("absent").to_str().unwrap().into();
    assert!(matches!(
        fixture.service.submit(request),
        Err(ProjectExecutionError::ProviderUnavailable { .. })
    ));
    let mut request = fixture.request();
    request.capability_id = "arbitrary.execute".into();
    assert!(matches!(
        fixture.service.submit(request),
        Err(ProjectExecutionError::ProviderUnavailable { .. })
    ));
    assert!(fixture.requests.try_recv().is_err());
}

#[test]
fn interactive_leaf_dispatches_before_queued_background_leaves() {
    let fixture = LocalArtifactFixture::new();
    // Occupies the single provider slot.
    let first = fixture.service.submit(fixture.request()).unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let mut background = Vec::new();
    for index in 0..3 {
        let mut request = fixture.request();
        request.idempotency_key = format!("background-{index}");
        request.input["name"] = json!("background_leaf");
        background.push(fixture.service.submit(request).unwrap());
    }
    let mut request = fixture.request();
    request.idempotency_key = "interactive".into();
    request.priority = ProjectExecutionPriority::Interactive;
    request.input["name"] = json!("interactive_leaf");
    let interactive = fixture.service.submit(request).unwrap();

    fixture.reply.send(fixture.output()).unwrap();
    // The interactive leaf jumps ahead of all three queued background leaves.
    let dispatched = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(dispatched.messages[0].content.contains("interactive_leaf"));
    fixture.reply.send(fixture.output()).unwrap();
    for _ in &background {
        let dispatched = fixture
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(dispatched.messages[0].content.contains("background_leaf"));
        fixture.reply.send(fixture.output()).unwrap();
    }
    assert_eq!(
        fixture.wait_terminal(&first).status,
        ProjectExecutionStatus::Succeeded
    );
    assert_eq!(
        fixture.wait_terminal(&interactive).status,
        ProjectExecutionStatus::Succeeded
    );
    for job in background {
        assert_eq!(
            fixture.wait_terminal(&job).status,
            ProjectExecutionStatus::Succeeded
        );
    }
}

#[test]
fn host_submit_accepts_explicit_and_default_priority() {
    let fixture = LocalArtifactFixture::new();
    let task = fixture.request();
    // Occupies the single provider slot.
    fixture.service.submit(task.clone()).unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    // Queued behind the active job: three background, then one interactive.
    for index in 0..3 {
        fixture
            .host
            .invoke(
                "builtin.knowledge",
                "runtime.project_execution",
                json!({
                    "operation":"submit", "project_root":task.project_root,
                    "capability_id":task.capability_id,
                    "idempotency_key":format!("host-background-{index}"),
                    "input":{"semantic_element_id":"fn:parse", "element_kind":"function", "name":"background_leaf", "path":"parser.rs", "start_line":1, "end_line":1, "content_fingerprint":"sha256:abc", "requested_artifact_kinds":["job","receives","outcome","effects"]},
                }),
            )
            .unwrap();
    }
    fixture
        .host
        .invoke(
            "builtin.knowledge",
            "runtime.project_execution",
            json!({
                "operation":"submit", "project_root":task.project_root,
                "capability_id":task.capability_id,
                "idempotency_key":"host-interactive",
                "priority":"interactive",
                "input":{"semantic_element_id":"fn:parse", "element_kind":"function", "name":"interactive_leaf", "path":"parser.rs", "start_line":1, "end_line":1, "content_fingerprint":"sha256:abc", "requested_artifact_kinds":["job","receives","outcome","effects"]},
            }),
        )
        .unwrap();

    fixture.reply.send(fixture.output()).unwrap();
    let dispatched = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(dispatched.messages[0].content.contains("interactive_leaf"));
    fixture.reply.send(fixture.output()).unwrap();
    for _ in 0..3 {
        let dispatched = fixture
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(dispatched.messages[0].content.contains("background_leaf"));
        fixture.reply.send(fixture.output()).unwrap();
    }
}

#[test]
fn plugin_host_submission_reaches_local_engine_without_mcp_and_returns_result() {
    let fixture = LocalArtifactFixture::new();
    let task = fixture.request();
    let job = fixture.host.invoke("builtin.knowledge", "runtime.project_execution", json!({
        "operation":"submit", "project_root":task.project_root, "capability_id":task.capability_id,
        "idempotency_key":task.idempotency_key, "input":task.input,
    })).unwrap();
    assert_eq!(job["requester_id"], "builtin.knowledge");
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    fixture.reply.send(fixture.output()).unwrap();
    let job: ProjectExecutionJob = serde_json::from_value(job).unwrap();
    fixture.wait_terminal(&job);
    let completed = fixture
        .host
        .invoke(
            "builtin.knowledge",
            "runtime.project_execution",
            json!({"operation":"status", "job_id":job.job_id}),
        )
        .unwrap();
    assert_eq!(completed["status"], "succeeded");
    assert_eq!(completed["output"]["semantic_element_id"], "fn:parse");
    assert!(
        fixture
            .host
            .invoke(
                "different-plugin",
                "runtime.project_execution",
                json!({"operation":"status", "job_id":job.job_id})
            )
            .is_err()
    );
}

#[test]
fn folder_submission_queues_leaves_without_overflowing_provider_queue() {
    let fixture = LocalArtifactFixture::new();
    let jobs: Vec<_> = (0..12)
        .map(|index| {
            let mut request = fixture.request();
            request.idempotency_key = format!("leaf-{index}");
            fixture.service.submit(request).unwrap()
        })
        .collect();
    for _ in &jobs {
        fixture
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        fixture.reply.send(fixture.output()).unwrap();
    }
    for job in jobs {
        assert_eq!(
            fixture.wait_terminal(&job).status,
            ProjectExecutionStatus::Succeeded
        );
    }
}

#[test]
fn cancelling_queued_leaf_skips_engine_and_allows_next_leaf() {
    let fixture = LocalArtifactFixture::new();
    let first = fixture.service.submit(fixture.request()).unwrap();
    fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let mut request = fixture.request();
    request.idempotency_key = "cancelled-leaf".into();
    request.input["name"] = json!("cancelled_leaf");
    let cancelled = fixture.service.submit(request).unwrap();
    assert_eq!(cancelled.status, ProjectExecutionStatus::Queued);
    fixture
        .service
        .cancel(&cancelled.requester_id, &cancelled.job_id)
        .unwrap();
    let mut request = fixture.request();
    request.idempotency_key = "next-leaf".into();
    request.input["name"] = json!("next_leaf");
    let next = fixture.service.submit(request).unwrap();
    fixture.reply.send(fixture.output()).unwrap();
    let dispatched = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(dispatched.messages[0].content.contains("next_leaf"));
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&first).status,
        ProjectExecutionStatus::Succeeded
    );
    assert_eq!(
        fixture.wait_terminal(&next).status,
        ProjectExecutionStatus::Succeeded
    );
    assert_eq!(
        fixture.wait_terminal(&cancelled).status,
        ProjectExecutionStatus::Cancelled
    );
}

#[test]
fn provider_rejecting_structured_output_degrades_to_prompt_only() {
    let fixture =
        LocalArtifactFixture::with_settings(1, LocalArtifactFixture::default_settings(), true);
    let job = fixture.service.submit(fixture.request()).unwrap();
    let structured = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(structured.options.response_format.is_some());
    let degraded = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(degraded.options.response_format, None);
    assert_eq!(degraded.options.max_output_tokens, None);
    assert_eq!(degraded.options.temperature, None);
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&job).status,
        ProjectExecutionStatus::Succeeded
    );
}

#[test]
fn configured_engine_uses_generation_settings_when_set() {
    let fixture = LocalArtifactFixture::with_settings(
        1,
        AppSettings {
            assistant_engine: AssistantEngine::Codex,
            assistant_model: Some("assistant-model".into()),
            generation_engine: Some(AssistantEngine::Cerebras),
            generation_model: Some("gen-model".into()),
            ..Default::default()
        },
        false,
    );
    let job = fixture.service.submit(fixture.request()).unwrap();
    let request = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(request.provider_id.as_deref(), Some("cerebras"));
    assert_eq!(request.model.as_deref(), Some("gen-model"));
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&job).status,
        ProjectExecutionStatus::Succeeded
    );
}

#[test]
fn configured_engine_falls_back_to_assistant_when_generation_unset() {
    let fixture = LocalArtifactFixture::with_settings(
        1,
        AppSettings {
            assistant_engine: AssistantEngine::Codex,
            assistant_model: Some("assistant-model".into()),
            ..Default::default()
        },
        false,
    );
    let job = fixture.service.submit(fixture.request()).unwrap();
    let request = fixture
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(request.provider_id.as_deref(), Some("codex"));
    assert_eq!(request.model.as_deref(), Some("assistant-model"));
    fixture.reply.send(fixture.output()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&job).status,
        ProjectExecutionStatus::Succeeded
    );
}

#[test]
fn transport_outage_trips_breaker_and_queues_jobs_until_cooldown_ends() {
    let fixture = LocalArtifactFixture::new();
    // Six consecutive transport-class resets: three attempts fail job one,
    // three fail job two, whose second attempt trips the breaker.
    fixture.fail_transport(6);
    let first = fixture.service.submit(fixture.request()).unwrap();
    assert_eq!(
        fixture.wait_terminal(&first).status,
        ProjectExecutionStatus::Failed
    );
    let health = fixture.health("cerebras");
    assert_eq!(health.consecutive_transport_failures, 3);
    assert!(health.cooldown_until.is_none());

    let second = {
        let mut request = fixture.request();
        request.idempotency_key = "breaker-second".into();
        fixture.service.submit(request).unwrap()
    };
    assert_eq!(
        fixture.wait_terminal(&second).status,
        ProjectExecutionStatus::Failed
    );
    let health = fixture.health("cerebras");
    assert!(health.cooldown_until.is_some(), "breaker must be tripped");
    assert_eq!(health.cooldown, BREAKER_COOLDOWN_BASE * 2);

    // A queued job is NOT failed while the provider cools down: nothing is
    // dispatched until the cooldown elapses, then it runs and succeeds.
    fixture.fail_transport(0);
    let queued = {
        let mut request = fixture.request();
        request.idempotency_key = "breaker-queued".into();
        fixture.service.submit(request).unwrap()
    };
    assert!(
        fixture.requests.try_recv().is_err(),
        "dispatched during cooldown"
    );
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        fixture.requests.try_recv().is_err(),
        "dispatched during cooldown"
    );
    fixture.reply.send(fixture.output()).unwrap();
    let completed = fixture.wait_terminal(&queued);
    assert_eq!(completed.status, ProjectExecutionStatus::Succeeded);

    // Success resets the breaker.
    let health = fixture.health("cerebras");
    assert_eq!(health.consecutive_transport_failures, 0);
    assert!(health.cooldown_until.is_none());
    assert_eq!(health.cooldown, BREAKER_COOLDOWN_BASE);
}
