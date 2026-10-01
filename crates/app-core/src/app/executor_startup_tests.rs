use super::configured_llm_registry;
use lumvise_db_core::LocalPersistence;
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{LlmMessage, LlmRequest};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct StartupCompletionHttp {
    requests: Mutex<Vec<Value>>,
}

impl LlmHttpClient for StartupCompletionHttp {
    fn get_json(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("startup must not wait for model discovery");
    }

    fn post_json(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("OpenRouter must use its streamed completion path");
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        self.requests.lock().unwrap().push(request.payload.clone());
        Ok(vec![format!(
            "data: {}\n\n",
            json!({"choices":[{
                "delta":{"content":"Hello"}, "finish_reason":"stop"
            }]})
        )])
    }
}

#[test]
fn saved_openrouter_answers_before_provider_discovery() {
    let persistence = LocalPersistence::in_memory().unwrap();
    crate::app::provider_settings::set_provider_api_key(&persistence, "openrouter", "test-key")
        .unwrap();
    let http = Arc::new(StartupCompletionHttp::default());
    let registry = configured_llm_registry(&persistence, http.clone()).unwrap();
    assert!(http.requests.lock().unwrap().is_empty());

    let response = registry
        .complete(
            "openrouter",
            &LlmRequest {
                messages: vec![LlmMessage {
                    role: "user".into(),
                    content: "Hello".into(),
                }],
                provider_id: Some("openrouter".into()),
                model: Some("saved-model".into()),
                conversation_id: Some("startup-conversation".into()),
                stream: false,
                provider_session_id: None,
                mcp_servers: Vec::new(),
                modality_inputs: Vec::new(),
                options: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(response.content, "Hello");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"], "saved-model");
    assert_eq!(requests[0]["session_id"], "startup-conversation");
}
