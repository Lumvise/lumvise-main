#[path = "cerebras_provider/support.rs"]
mod tool_bridge_support;

use lumvise_neural_core::llm_providers::command_runner::{
    LlmCommandTransport, ProviderCommandOutput,
};
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{LlmMcpServerConfig, LlmRequest};
use lumvise_neural_core::{
    LlmProviderAvailability, LlmProviderCandidate, LlmProviderConfig, LlmProviderKind,
    LlmProviderRegistry,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::result::Result as StdResult;
use std::sync::Arc;

type Result<T, E = lumvise_neural_core::NeuralError> = StdResult<T, E>;

#[derive(Clone, Default)]
struct CustomFakeHttpClient {
    calls: Arc<Mutex<Vec<LlmHttpRequest>>>,
    inventory: Arc<Mutex<Option<Value>>>,
    responses: Arc<Mutex<VecDeque<Value>>>,
}

impl LlmHttpClient for CustomFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().push(request.clone());
        if let Some(response) = self.responses.lock().pop_front() {
            return Ok(response);
        }
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

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        let response = self.post_json(request)?;
        let mut delta = response["choices"][0]["message"].clone();
        if let Some(calls) = delta["tool_calls"].as_array_mut() {
            for (index, call) in calls.iter_mut().enumerate() {
                call["index"] = json!(index);
            }
        }
        Ok(vec![format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":delta,"finish_reason":"stop"}]})
        )])
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

fn custom_tool_responses(rounds: usize) -> VecDeque<Value> {
    (0..rounds)
        .map(|round| {
            json!({"choices":[{"message":{"role":"assistant","content":null,
                "tool_calls":[{"id":format!("call-{round}"),"type":"function",
                    "function":{"name":tool_bridge_support::CEREBRAS_TEST_TOOL_NAME,
                        "arguments":"{\"query\":\"synthetic fixture\"}"}}]}}]})
        })
        .collect()
}

fn custom_tool_request(server: &tool_bridge_support::FakeMcpServer) -> LlmRequest {
    let mut request = custom_request();
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "synthetic-tools".into(),
        url: format!("{}/mcp", server.url()),
    }];
    request
}

#[test]
fn custom_openai_completes_visual_work_after_nine_tool_rounds() {
    let server = tool_bridge_support::FakeMcpServer::spawn(10);
    let http_client = CustomFakeHttpClient::default();
    *http_client.responses.lock() = custom_tool_responses(9);
    let registry =
        LlmProviderRegistry::from_configs(vec![custom_config()], Arc::new(http_client.clone()))
            .unwrap();
    let response = registry
        .complete("custom_openai", &custom_tool_request(&server))
        .unwrap();
    assert_eq!(response.content, "custom reply");
    assert_eq!(server.tool_calls().len(), 9);
    let calls = http_client.calls.lock();
    assert_eq!(calls.len(), 10);
    assert!(calls.iter().all(|call| call.payload["tools"].is_array()));
}

#[test]
fn custom_openai_stops_at_the_tool_round_budget_without_an_extra_call() {
    let server = tool_bridge_support::FakeMcpServer::spawn(33);
    let http_client = CustomFakeHttpClient::default();
    *http_client.responses.lock() = custom_tool_responses(32);
    let registry =
        LlmProviderRegistry::from_configs(vec![custom_config()], Arc::new(http_client.clone()))
            .unwrap();
    let error = registry
        .complete("custom_openai", &custom_tool_request(&server))
        .unwrap_err();
    assert!(matches!(
        error,
        lumvise_neural_core::NeuralError::ToolRoundsExhausted { .. }
    ));
    assert_eq!(server.tool_calls().len(), 32);
    assert_eq!(http_client.calls.lock().len(), 32);
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
