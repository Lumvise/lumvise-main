use super::*;

use lumvise_frontend_core::LinkClassificationProvider;
use lumvise_neural_core::llm_providers::LlmRequest;
use lumvise_neural_core::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmStreamEventSink,
};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, NeuralError};
use lumvise_plugin_runtime::{PluginInvocationClass, PluginInvocationContext};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use crate::plugin::LinkDecisionCredentials;
use crate::plugin::plugin_host_capabilities::LinkDecisionService;

struct FakeDecisionHttpClient {
    requests: Mutex<Vec<LlmHttpRequest>>,
    response: Value,
}

impl FakeDecisionHttpClient {
    fn returning(response: Value) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            response,
        }
    }
}

impl LlmHttpClient for FakeDecisionHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.response.clone())
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        Err(NeuralError::ProviderFailed {
            provider_id: request.endpoint.clone(),
            message: "dispatch fake does not stream".into(),
        })
    }
}

struct FakeDecisionCredentials {
    calls: AtomicUsize,
}

impl FakeDecisionCredentials {
    fn configured() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn config() -> LlmProviderConfig {
        LlmProviderConfig {
            provider_id: "openrouter".into(),
            kind: LlmProviderKind::OpenRouter,
            model: "provider-default".into(),
            endpoint: None,
            credential: Some("dispatch-test-key".into()),
            completion_concurrency: None,
            spawn: None,
        }
    }
}

impl LinkDecisionCredentials for FakeDecisionCredentials {
    fn openrouter_config(
        &self,
        _control: &InvocationControl,
    ) -> Result<Option<LlmProviderConfig>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Self::config()))
    }
}

struct FakeCancellationObservingCredentials {
    entered: Sender<()>,
    observed: Arc<AtomicBool>,
}

impl LinkDecisionCredentials for FakeCancellationObservingCredentials {
    fn openrouter_config(
        &self,
        control: &InvocationControl,
    ) -> Result<Option<LlmProviderConfig>, String> {
        self.entered
            .send(())
            .map_err(|_| "dispatch test stopped before credential lookup".to_string())?;
        while !control.is_cancelled() && !control.is_expired() {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.observed
            .store(control.is_cancelled(), Ordering::SeqCst);
        Ok(Some(FakeDecisionCredentials::config()))
    }
}

struct FakeCloudDecisionProvider {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl LlmProvider for FakeCloudDecisionProvider {
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

    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(LlmResponse {
            provider_id: self.provider_id().into(),
            model: "cloud-dispatch-model".into(),
            content: r#"{"decision":"hold"}"#.into(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        _on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        Ok(())
    }
}

fn setup_services(
    preference: LinkClassificationProvider,
    credentials: Arc<dyn LinkDecisionCredentials>,
) -> (
    Arc<PluginHostServices>,
    Arc<FakeDecisionHttpClient>,
    Arc<Mutex<Vec<LlmRequest>>>,
) {
    let cloud_requests = Arc::new(Mutex::new(Vec::new()));
    let registry = LlmProviderRegistry::from_provider_instances(vec![
        Box::new(ConfiguredAssistantProvider),
        Box::new(FakeCloudDecisionProvider {
            requests: Arc::clone(&cloud_requests),
        }),
    ])
    .expect("decision and Assistant providers register");
    let mut frontend = FrontendCore::default();
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantEngine(
        AssistantEngine::Cerebras,
    ));
    frontend.apply_app_settings_patch(&AppSettingsPatch::AssistantModel(Some(
        "assistant-kept-model".into(),
    )));
    frontend.apply_app_settings_patch(&AppSettingsPatch::LinkClassificationProvider(preference));

    let services = PluginHostServices::new(frontend, registry, test_persistence());
    let http = Arc::new(FakeDecisionHttpClient::returning(json!({
        "model": "typesafe/jev-1.13",
        "answers": { "decision": { "type": "choice", "choice": "propagate" } }
    })));
    let http_client: Arc<dyn LlmHttpClient> = http.clone();
    let service = LinkDecisionService::new(services.llm_executors(), http_client, credentials);
    *services.link_decisions.lock().unwrap() = Some(Arc::new(service));
    (services, http, cloud_requests)
}

fn invocation_context() -> PluginInvocationContext {
    PluginInvocationContext::new(
        "link-decision-dispatch-test",
        "builtin.agents",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(5),
    )
}

#[test]
fn controlled_dispatch_uses_semantic_preference_and_preserves_assistant_selection() {
    for (preference, expected_decision) in [
        (LinkClassificationProvider::Jev, "propagate"),
        (LinkClassificationProvider::LumviseCloud, "hold"),
    ] {
        let credentials = Arc::new(FakeDecisionCredentials::configured());
        let credential_source: Arc<dyn LinkDecisionCredentials> = credentials.clone();
        let (services, http, cloud_requests) = setup_services(preference, credential_source);
        let frontend = services.frontend();
        let before = frontend.lock().unwrap().app_settings().clone();
        let result = services
            .invoke_controlled(
                "builtin.agents",
                NEURAL_DECISIONS,
                json!({
                    "cause": "propagate meaningful changes",
                    "baseline": "before",
                    "current": "after"
                }),
                &invocation_context(),
            )
            .expect("selected link decision provider dispatches");

        assert_eq!(result["provider"], preference.value());
        assert_eq!(result["decision"], expected_decision);
        assert_eq!(
            result["model"],
            match preference {
                LinkClassificationProvider::Jev => "typesafe/jev-1.13",
                LinkClassificationProvider::LumviseCloud => "cloud-dispatch-model",
            }
        );
        let after = frontend.lock().unwrap().app_settings().clone();
        assert_eq!(after.assistant_engine, AssistantEngine::Cerebras);
        assert_eq!(
            after.assistant_model.as_deref(),
            Some("assistant-kept-model")
        );
        assert_eq!(after.assistant_engine, before.assistant_engine);
        assert_eq!(after.assistant_model, before.assistant_model);

        match preference {
            LinkClassificationProvider::Jev => {
                let requests = http.requests.lock().unwrap();
                assert_eq!(requests.len(), 1);
                assert_eq!(
                    requests[0].endpoint,
                    "https://openrouter.ai/api/alpha/decisions"
                );
                assert!(cloud_requests.lock().unwrap().is_empty());
                assert_eq!(credentials.calls.load(Ordering::SeqCst), 1);
            }
            LinkClassificationProvider::LumviseCloud => {
                assert!(http.requests.lock().unwrap().is_empty());
                assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
                let requests = cloud_requests.lock().unwrap();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].provider_id.as_deref(), Some("lumvise_cloud"));
                assert_eq!(requests[0].model, None);
            }
        }
    }
}

#[test]
fn controlled_dispatch_forwards_original_cancellation_to_credentials() {
    let (entered, credential_lookup) = mpsc::channel();
    let cancellation_observed = Arc::new(AtomicBool::new(false));
    let credentials: Arc<dyn LinkDecisionCredentials> =
        Arc::new(FakeCancellationObservingCredentials {
            entered,
            observed: Arc::clone(&cancellation_observed),
        });
    let (services, http, _) = setup_services(LinkClassificationProvider::Jev, credentials);
    let context = invocation_context();
    let cancellation = context.cancellation();
    let dispatch_services = Arc::clone(&services);
    let dispatch_context = context.clone();
    let dispatch = std::thread::spawn(move || {
        dispatch_services.invoke_controlled(
            "builtin.agents",
            NEURAL_DECISIONS,
            json!({
                "cause": "propagate meaningful changes",
                "baseline": "before",
                "current": "after"
            }),
            &dispatch_context,
        )
    });
    credential_lookup
        .recv_timeout(Duration::from_secs(2))
        .expect("credential lookup started with the invocation control");
    cancellation.cancel();
    let error = dispatch
        .join()
        .expect("controlled dispatch worker joins")
        .unwrap_err();

    assert!(error.to_string().contains("cancelled"));
    assert!(cancellation_observed.load(Ordering::SeqCst));
    assert!(http.requests.lock().unwrap().is_empty());
}
