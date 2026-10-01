use lumvise_neural_core::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmTextChunkSink,
};
use lumvise_neural_core::llm_providers::{
    LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, Result};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ZAiFakeHttpClient {
    calls: Arc<Mutex<Vec<LlmHttpRequest>>>,
    completion_response: Value,
    stream_chunks: Vec<String>,
}

impl ZAiFakeHttpClient {
    fn completion(response: Value) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: response,
            stream_chunks: Vec::new(),
        }
    }

    fn stream(chunks: Vec<String>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: json!({}),
            stream_chunks: chunks,
        }
    }

    fn calls(&self) -> Vec<LlmHttpRequest> {
        self.calls.lock().unwrap().clone()
    }
}

impl LlmHttpClient for ZAiFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(self.completion_response.clone())
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(self.stream_chunks.clone())
    }

    fn stream_text_with_chunks(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut LlmTextChunkSink<'_>,
    ) -> Result<()> {
        self.calls.lock().unwrap().push(request.clone());
        for chunk in &self.stream_chunks {
            on_chunk(chunk.clone())?;
        }
        Ok(())
    }
}

#[test]
fn z_ai_complete_posts_chat_completions_payload_and_parses_message_content() {
    let http_client = ZAiFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "glm reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());

    let response = registry.complete("z_ai", &z_ai_request(false)).unwrap();

    assert_eq!(response.content, "glm reply");
    assert_eq!(http_client.calls(), vec![expected_call(false)]);
}

#[test]
fn z_ai_stream_parses_sse_deltas_and_done() {
    let http_client = ZAiFakeHttpClient::stream(vec![
        sse_data(json!({ "choices": [{ "delta": { "content": "gl" } }] })),
        sse_data(json!({ "choices": [{ "delta": { "content": "m" } }] })),
        "data: [DONE]\n\n".to_string(),
    ]);
    let registry = registry_with_client(http_client.clone());

    let events = registry
        .stream("z_ai", &z_ai_request(true), StreamControl::unbounded())
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "gl".to_string()
            },
            LlmStreamEvent::ContentDelta {
                text: "m".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
    assert_eq!(http_client.calls(), vec![expected_call(true)]);
}

#[test]
fn z_ai_rejects_image_and_live_inputs() {
    let image_error = z_ai_modality_error(LlmModalityInputKind::ImageSnapshot);
    let audio_error = z_ai_modality_error(LlmModalityInputKind::LiveAudioChunk);

    assert!(image_error.contains("z_ai:input-1"));
    assert!(image_error.contains("image_snapshot_input is unsupported"));
    assert!(audio_error.contains("live_audio_input is unsupported"));
}

fn registry_with_client(http_client: ZAiFakeHttpClient) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(vec![z_ai_config()], Arc::new(http_client)).unwrap()
}

fn z_ai_config() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "z_ai".to_string(),
        kind: LlmProviderKind::Zai,
        model: "glm-5.2".to_string(),
        endpoint: Some("https://api.z.ai/api/coding/paas/v4".to_string()),
        credential: Some("zai-token".to_string()),
        completion_concurrency: None,
        spawn: None,
    }
}

fn z_ai_request(stream: bool) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

fn expected_call(stream: bool) -> LlmHttpRequest {
    LlmHttpRequest {
        timeout: None,
        endpoint: "https://api.z.ai/api/coding/paas/v4/chat/completions".to_string(),
        credential: "zai-token".to_string(),
        headers: Default::default(),
        payload: json!({
            "model": "glm-5.2",
            "stream": stream,
            "messages": [{ "role": "user", "content": "hello" }],
        }),
    }
}

fn sse_data(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn z_ai_modality_error(kind: LlmModalityInputKind) -> String {
    let registry = registry_with_client(ZAiFakeHttpClient::completion(json!({})));
    let mut request = z_ai_request(false);
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "input-1".to_string(),
        kind,
        media_type: "image/png".to_string(),
        bytes: vec![137, 80],
        metadata: json!({}),
    }];

    registry
        .complete("z_ai", &request)
        .err()
        .unwrap()
        .to_string()
}
