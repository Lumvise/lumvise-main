use lumvise_neural_core::llm_providers::LlmRequest;
use lumvise_neural_core::llm_providers::command_runner::{
    LlmCommandTransport, ProviderCommandOutput,
};
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::{
    LlmProviderAvailability, LlmProviderCandidate, LlmProviderConfig, LlmProviderKind,
    LlmProviderRegistry,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::result::Result as StdResult;
use std::sync::Arc;

type Result<T, E = lumvise_neural_core::NeuralError> = StdResult<T, E>;

#[derive(Clone, Default)]
struct CustomFakeHttpClient {
    calls: Arc<Mutex<Vec<LlmHttpRequest>>>,
    inventory: Arc<Mutex<Option<Value>>>,
}

impl LlmHttpClient for CustomFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().push(request.clone());
        Ok(json!({
            "choices": [{ "message": { "content": "custom reply" } }]
        }))
    }

    fn get_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().push(request.clone());
        Ok(self
            .inventory
            .lock()
            .clone()
            .unwrap_or_else(|| json!({"data": [{"id": "qwen3"}]})))
    }

    fn stream_text(&self, _: &LlmHttpRequest) -> Result<Vec<String>> {
        unreachable!("stream_text is not used by these tests")
    }
}

struct NoCommands;

impl LlmCommandTransport for NoCommands {
    fn run(
        &self,
        _: &lumvise_neural_core::SpawnConfig,
        _: Vec<String>,
        _: Option<&str>,
    ) -> Result<ProviderCommandOutput> {
        Err(lumvise_neural_core::NeuralError::ProviderFailed {
            provider_id: "custom_openai".into(),
            message: "no CLI transport is expected for a custom endpoint".into(),
        })
    }
}

fn custom_config() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "custom_openai".into(),
        kind: LlmProviderKind::OpenAiCompatible,
        model: "provider-default".into(),
        endpoint: Some("http://localhost:11434/v1".into()),
        credential: None,
        completion_concurrency: None,
        spawn: None,
    }
}

fn custom_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![lumvise_neural_core::llm_providers::LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream: false,
        provider_id: Some("custom_openai".to_string()),
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

#[test]
fn custom_openai_config_validates_without_credential() {
    assert!(custom_config().validate().is_ok());
}

#[test]
fn custom_openai_config_without_endpoint_is_rejected() {
    let mut config = custom_config();
    config.endpoint = None;
    assert!(config.validate().is_err());
}

#[test]
fn custom_openai_complete_posts_chat_completions_without_authorization() {
    let http_client = CustomFakeHttpClient::default();
    let registry =
        LlmProviderRegistry::from_configs(vec![custom_config()], Arc::new(http_client.clone()))
            .unwrap();

    let response = registry
        .complete("custom_openai", &custom_request())
        .unwrap();

    assert_eq!(response.content, "custom reply");
    let calls = http_client.calls.lock().clone();
    let completion = calls
        .iter()
        .find(|call| call.endpoint.ends_with("/chat/completions"))
        .expect("chat completion request");
    assert_eq!(
        completion.endpoint,
        "http://localhost:11434/v1/chat/completions"
    );
    // An empty credential means no Authorization header on the wire; the
    // request carries no credential so ReqwestLlmHttpClient omits the header.
    assert_eq!(completion.credential, "");
    assert_eq!(
        completion
            .headers
            .get("Authorization")
            .or_else(|| completion.headers.get("authorization")),
        None
    );
}

#[test]
fn custom_openai_sync_discovers_models_from_inventory_endpoint() {
    let http_client = CustomFakeHttpClient::default();
    *http_client.inventory.lock() = Some(json!({
        "data": [{ "id": "qwen3" }, { "id": "llama3" }]
    }));

    let synchronizer = lumvise_neural_core::LlmProviderSynchronizer::new(
        Arc::new(http_client.clone()),
        Arc::new(NoCommands),
        lumvise_neural_core::llm_providers::model_catalog::ProviderModelCatalog::configured()
            .unwrap(),
    );
    let sync = synchronizer
        .sync(vec![LlmProviderCandidate::Configured(custom_config())])
        .unwrap();

    let status = sync
        .catalog
        .providers
        .iter()
        .find(|status| status.provider_id == "custom_openai")
        .unwrap();
    assert_eq!(status.kind, LlmProviderKind::OpenAiCompatible);
    assert_eq!(status.state, LlmProviderAvailability::Available);
    let discovery_calls = http_client
        .calls
        .lock()
        .iter()
        .filter(|call| call.endpoint == "http://localhost:11434/v1/models")
        .count();
    assert!(
        discovery_calls >= 1,
        "discovery must query {{endpoint}}/models"
    );
    // The discovered model is selected as provider-default.
    let completed = sync.registry.complete("custom_openai", &custom_request());
    assert!(
        completed.is_ok(),
        "registry must be usable after discovery: {completed:?}"
    );
}
