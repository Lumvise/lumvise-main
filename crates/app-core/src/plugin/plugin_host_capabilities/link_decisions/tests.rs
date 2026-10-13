use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use lumvise_frontend_core::LinkClassificationProvider;
use lumvise_neural_core::{
    LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, NeuralError,
    llm_providers::{
        LlmCapabilitySupport, LlmExecutorRegistry, LlmProviderCapabilities, LlmRequest,
        LlmResponse,
        contract::{
            LlmHttpClient, LlmHttpRequest, LlmProvider, LlmStreamEventSink, ProviderCallControl,
        },
    },
    process::StreamControl,
};
use lumvise_plugin_runtime::HostCapabilityError;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use super::service::production_http;
use super::{LinkDecisionCredentials, LinkDecisionService};

#[derive(Default)]
struct FakeDecisionCredentials {
    config: Mutex<Option<LlmProviderConfig>>,
    calls: AtomicUsize,
}

impl FakeDecisionCredentials {
    fn configured(endpoint: Option<&str>) -> Self {
        Self {
            config: Mutex::new(Some(LlmProviderConfig {
                provider_id: "openrouter".into(),
                kind: LlmProviderKind::OpenRouter,
                model: "provider-default".into(),
                endpoint: endpoint.map(str::to_string),
                credential: Some("test-openrouter-key".into()),
                completion_concurrency: None,
                spawn: None,
            })),
            calls: AtomicUsize::new(0),
        }
    }
}

impl LinkDecisionCredentials for FakeDecisionCredentials {
    fn openrouter_config(
        &self,
        _control: &InvocationControl,
    ) -> Result<Option<LlmProviderConfig>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.config.lock().unwrap().clone())
    }
}

struct FakeCancellingDecisionCredentials;

impl LinkDecisionCredentials for FakeCancellingDecisionCredentials {
    fn openrouter_config(
        &self,
        control: &InvocationControl,
    ) -> Result<Option<LlmProviderConfig>, String> {
        control.cancel();
        Ok(Some(LlmProviderConfig {
            provider_id: "openrouter".into(),
            kind: LlmProviderKind::OpenRouter,
            model: "provider-default".into(),
            endpoint: None,
            credential: Some("test-openrouter-key".into()),
            completion_concurrency: None,
            spawn: None,
        }))
    }
}

#[derive(Default)]
struct FakeDecisionHttpClient {
    requests: Mutex<Vec<LlmHttpRequest>>,
    response: Mutex<Value>,
}

impl FakeDecisionHttpClient {
    fn responding(response: Value) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(response),
        }
    }
}

impl LlmHttpClient for FakeDecisionHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.response.lock().unwrap().clone())
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        Err(NeuralError::ProviderFailed {
            provider_id: request.endpoint.clone(),
            message: "fake transport does not stream".into(),
        })
    }
}

struct FakeDecisionLlmProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    content: String,
    model: String,
    fail: bool,
}

impl FakeDecisionLlmProvider {
    fn returning(content: &str, model: &str) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            content: content.into(),
            model: model.into(),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            content: String::new(),
            model: String::new(),
            fail: true,
        }
    }
}

impl LlmProvider for FakeDecisionLlmProvider {
    fn provider_id(&self) -> &str {
        "lumvise_cloud"
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: self.provider_id().into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Unsupported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, _request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        unreachable!("executor uses complete_controlled")
    }

    fn complete_controlled(
        &self,
        request: &LlmRequest,
        _control: &dyn ProviderCallControl,
    ) -> lumvise_neural_core::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        if self.fail {
            return Err(NeuralError::ProviderFailed {
                provider_id: self.provider_id().into(),
                message: "provider detail must not cross the capability boundary".into(),
            });
        }
        Ok(LlmResponse {
            provider_id: self.provider_id().into(),
            model: self.model.clone(),
            content: self.content.clone(),
            metadata: Value::Null,
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        unreachable!("link-decision requests never stream")
    }
}

fn make_service(
    http: Arc<FakeDecisionHttpClient>,
    credentials: Arc<FakeDecisionCredentials>,
    provider: Option<Box<dyn LlmProvider>>,
) -> LinkDecisionService {
    let registry = provider
        .map(|provider| LlmProviderRegistry::from_provider_instances(vec![provider]).unwrap())
        .unwrap_or_else(LlmProviderRegistry::empty);
    LinkDecisionService::new(
        LlmExecutorRegistry::new(Arc::new(Mutex::new(registry))),
        http,
        credentials,
    )
}

fn decide(
    service: &LinkDecisionService,
    provider: LinkClassificationProvider,
    input: Value,
    control: InvocationControl,
) -> Result<Value, HostCapabilityError> {
    service.decide(provider, input, control)
}

fn request(cause: &str) -> Value {
    json!({
        "cause": cause,
        "baseline": "old artifact",
        "current": "new artifact",
    })
}

fn jev_answer(choice: &str) -> Value {
    json!({
        "model": "typesafe/jev-1.13",
        "answers": { "decision": { "type": "choice", "choice": choice } }
    })
}

#[test]
fn jev_posts_choice_state_to_default_decision_endpoint_with_bearer_key() {
    for (choice, expected) in [("propagate", "propagate"), ("hold", "hold")] {
        let http = Arc::new(FakeDecisionHttpClient::responding(jev_answer(choice)));
        let credentials = Arc::new(FakeDecisionCredentials::configured(None));
        let service = make_service(Arc::clone(&http), Arc::clone(&credentials), None);
        let output = decide(
            &service,
            LinkClassificationProvider::Jev,
            request("publish after meaningful edits"),
            InvocationControl::sixty_seconds(),
        )
        .unwrap();
        assert_eq!(output["decision"], expected);
        assert_eq!(output["model"], "typesafe/jev-1.13");
        let calls = http.requests.lock().unwrap();
        let sent = &calls[0];
        assert_eq!(sent.endpoint, "https://openrouter.ai/api/alpha/decisions");
        assert_eq!(sent.credential, "test-openrouter-key");
        assert_eq!(sent.payload["model"], "typesafe/jev-1.13");
        assert_eq!(sent.payload["state"]["baseline"], "old artifact");
        assert_eq!(sent.payload["state"]["current"], "new artifact");
        assert_eq!(sent.payload["questions"]["decision"]["type"], "choice");
        assert!(
            sent.payload["questions"]["decision"]["instructions"]
                .as_str()
                .unwrap()
                .contains("publish after meaningful edits")
        );
    }
}

#[test]
fn jev_endpoint_keeps_custom_prefix_and_accepts_trailing_slashes() {
    for (base, expected) in [
        (
            "https://openrouter.ai/api/v1/",
            "https://openrouter.ai/api/alpha/decisions",
        ),
        (
            "https://gateway.example/custom/api/v1/",
            "https://gateway.example/custom/api/alpha/decisions",
        ),
    ] {
        let http = Arc::new(FakeDecisionHttpClient::responding(jev_answer("hold")));
        let credentials = Arc::new(FakeDecisionCredentials::configured(Some(base)));
        let service = make_service(Arc::clone(&http), credentials, None);
        decide(
            &service,
            LinkClassificationProvider::Jev,
            request("condition"),
            InvocationControl::sixty_seconds(),
        )
        .unwrap();
        assert_eq!(http.requests.lock().unwrap()[0].endpoint, expected);
    }
}

#[test]
fn jev_rejects_malformed_endpoint_and_malformed_answer() {
    let http = Arc::new(FakeDecisionHttpClient::responding(jev_answer("propagate")));
    let credentials = Arc::new(FakeDecisionCredentials::configured(Some(
        "https://example.com/not-openrouter",
    )));
    let service = make_service(Arc::clone(&http), credentials, None);
    let error = decide(
        &service,
        LinkClassificationProvider::Jev,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("invalid_openrouter_endpoint"));
    assert!(http.requests.lock().unwrap().is_empty());

    let http = Arc::new(FakeDecisionHttpClient::responding(json!({
        "answers": { "decision": { "type": "text", "choice": "propagate" } }
    })));
    let service = make_service(
        Arc::clone(&http),
        Arc::new(FakeDecisionCredentials::configured(None)),
        None,
    );
    let error = decide(
        &service,
        LinkClassificationProvider::Jev,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("malformed_provider_response"));
}

#[test]
fn jev_requires_signed_in_openrouter_credentials_without_leaking_key() {
    let http = Arc::new(FakeDecisionHttpClient::default());
    let signed_out = Arc::new(FakeDecisionCredentials::default());
    let service = make_service(Arc::clone(&http), Arc::clone(&signed_out), None);
    let error = decide(
        &service,
        LinkClassificationProvider::Jev,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("authentication_required"));
    assert!(http.requests.lock().unwrap().is_empty());

    let missing_key = Arc::new(FakeDecisionCredentials::configured(None));
    missing_key
        .config
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .credential = None;
    let service = make_service(Arc::clone(&http), missing_key, None);
    let error = decide(
        &service,
        LinkClassificationProvider::Jev,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("provider_unconfigured"));
    assert!(!error.to_string().contains("test-openrouter-key"));
}

#[test]
fn jev_propagates_cancellation_from_credential_lookup_before_http() {
    let http = Arc::new(FakeDecisionHttpClient::responding(jev_answer("hold")));
    let http_client: Arc<dyn LlmHttpClient> = http.clone();
    let service = LinkDecisionService::new(
        LlmExecutorRegistry::new(Arc::new(Mutex::new(LlmProviderRegistry::empty()))),
        http_client,
        Arc::new(FakeCancellingDecisionCredentials),
    );

    let error = decide(
        &service,
        LinkClassificationProvider::Jev,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("cancelled"));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn cloud_uses_explicit_provider_strict_schema_and_preserves_artifact_state() {
    let provider =
        FakeDecisionLlmProvider::returning(r#"{"decision":"propagate"}"#, "cloud-model-7");
    let provider_ref = ProviderProbe::from_fake(&provider);
    let http = Arc::new(FakeDecisionHttpClient::default());
    let credentials = Arc::new(FakeDecisionCredentials::default());
    let service = make_service(
        http.clone(),
        Arc::clone(&credentials),
        Some(Box::new(provider)),
    );
    let output = decide(
        &service,
        LinkClassificationProvider::LumviseCloud,
        request("publish after meaningful edits"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert_eq!(output["decision"], "propagate");
    assert_eq!(output["provider"], "lumvise_cloud");
    assert_eq!(output["model"], "cloud-model-7");
    assert_eq!(provider_ref.calls.load(Ordering::SeqCst), 1);
    assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
    assert!(http.requests.lock().unwrap().is_empty());
    let requests = provider_ref.requests.lock().unwrap();
    let sent = &requests[0];
    assert_eq!(sent.provider_id.as_deref(), Some("lumvise_cloud"));
    assert_eq!(sent.model, None);
    assert!(!sent.stream);
    assert_eq!(sent.messages[0].role, "system");
    assert_eq!(sent.messages[1].role, "user");
    let state: Value = serde_json::from_str(&sent.messages[1].content).unwrap();
    assert_eq!(state, request("publish after meaningful edits"));
    let Some(lumvise_neural_core::llm_providers::LlmResponseFormat::JsonSchema {
        name,
        schema,
        strict,
    }) = &sent.options.response_format
    else {
        panic!("expected strict JSON Schema response format")
    };
    assert_eq!(name, "link_decision");
    assert!(*strict);
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["decision"]["enum"],
        json!(["propagate", "hold"])
    );
}

struct ProviderProbe {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ProviderProbe {
    fn from_fake(provider: &FakeDecisionLlmProvider) -> Self {
        Self {
            calls: Arc::clone(&provider.calls),
            requests: Arc::clone(&provider.requests),
        }
    }
}

#[test]
fn cloud_rejects_malformed_answers_and_provider_failures_without_raw_details() {
    for content in ["I think propagate", r#"{"decision":"maybe"}"#] {
        let provider = FakeDecisionLlmProvider::returning(content, "cloud-model");
        let service = make_service(
            Arc::new(FakeDecisionHttpClient::default()),
            Arc::new(FakeDecisionCredentials::default()),
            Some(Box::new(provider)),
        );
        let error = decide(
            &service,
            LinkClassificationProvider::LumviseCloud,
            request("condition"),
            InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("malformed_provider_response"));
    }

    let provider = FakeDecisionLlmProvider::failing();
    let service = make_service(
        Arc::new(FakeDecisionHttpClient::default()),
        Arc::new(FakeDecisionCredentials::default()),
        Some(Box::new(provider)),
    );
    let error = decide(
        &service,
        LinkClassificationProvider::LumviseCloud,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("provider_rejected"));
    assert!(!text.contains("provider detail"));
}

#[test]
fn cloud_uses_provider_default_model_when_response_has_no_model_metadata() {
    let provider = FakeDecisionLlmProvider::returning(r#"{"decision":"hold"}"#, "");
    let service = make_service(
        Arc::new(FakeDecisionHttpClient::default()),
        Arc::new(FakeDecisionCredentials::default()),
        Some(Box::new(provider)),
    );
    let output = decide(
        &service,
        LinkClassificationProvider::LumviseCloud,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert_eq!(output["model"], "provider-default");
}

#[test]
fn jev_uses_fixed_model_when_response_has_no_model_metadata() {
    let http = Arc::new(FakeDecisionHttpClient::responding(json!({
        "answers": { "decision": { "type": "choice", "choice": "hold" } }
    })));
    let service = make_service(
        Arc::clone(&http),
        Arc::new(FakeDecisionCredentials::configured(None)),
        None,
    );
    let output = decide(
        &service,
        LinkClassificationProvider::Jev,
        request("condition"),
        InvocationControl::sixty_seconds(),
    )
    .unwrap();
    assert_eq!(output["model"], "typesafe/jev-1.13");
}

#[test]
fn cancelled_and_expired_invocations_do_not_dispatch_http_or_executor_calls() {
    let http = Arc::new(FakeDecisionHttpClient::responding(jev_answer("hold")));
    let service = make_service(
        Arc::clone(&http),
        Arc::new(FakeDecisionCredentials::configured(None)),
        None,
    );
    let cancelled = InvocationControl::sixty_seconds();
    cancelled.cancel();
    assert!(
        decide(
            &service,
            LinkClassificationProvider::Jev,
            request("condition"),
            cancelled,
        )
        .is_err()
    );
    assert!(http.requests.lock().unwrap().is_empty());

    let provider = FakeDecisionLlmProvider::returning(r#"{"decision":"hold"}"#, "cloud-model");
    let calls = Arc::new(AtomicUsize::new(0));
    let service = make_service(
        Arc::new(FakeDecisionHttpClient::default()),
        Arc::new(FakeDecisionCredentials::default()),
        Some(Box::new(CountingProvider::new(
            provider,
            Arc::clone(&calls),
        ))),
    );
    assert!(
        decide(
            &service,
            LinkClassificationProvider::LumviseCloud,
            request("condition"),
            InvocationControl::with_deadline(Duration::ZERO),
        )
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct CountingProvider {
    inner: FakeDecisionLlmProvider,
    calls: Arc<AtomicUsize>,
}

impl CountingProvider {
    fn new(inner: FakeDecisionLlmProvider, calls: Arc<AtomicUsize>) -> Self {
        Self { inner, calls }
    }
}

impl LlmProvider for CountingProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }
    fn capabilities(&self) -> LlmProviderCapabilities {
        self.inner.capabilities()
    }
    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        self.inner.complete(request)
    }
    fn complete_controlled(
        &self,
        request: &LlmRequest,
        control: &dyn ProviderCallControl,
    ) -> lumvise_neural_core::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.complete_controlled(request, control)
    }
    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        sink: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        self.inner.stream_with_events(request, control, sink)
    }
}

#[test]
fn malformed_root_input_and_empty_cause_fail_before_provider_work() {
    let http = Arc::new(FakeDecisionHttpClient::default());
    let credentials = Arc::new(FakeDecisionCredentials::configured(None));
    let service = make_service(Arc::clone(&http), credentials, None);
    for input in [
        json!({"cause": ""}),
        json!({"cause": "condition", "extra": true}),
    ] {
        let error = service
            .decide(
                LinkClassificationProvider::Jev,
                input,
                InvocationControl::sixty_seconds(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("invalid_host_capability_input"));
    }
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn production_http_constructor_returns_lazy_client_without_async_panic() {
    let http = production_http();
    assert_eq!(Arc::strong_count(&http), 1);
}
