use lumvise_neural_core::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmTextChunkSink,
};
use lumvise_neural_core::llm_providers::{
    LlmMcpServerConfig, LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest,
    LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{
    LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, NeuralError, Result,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone)]
struct OpenRouterFakeHttpClient {
    calls: Arc<Mutex<Vec<LlmHttpRequest>>>,
    completion_response: Value,
    completion_responses: Arc<Mutex<VecDeque<Value>>>,
    stream_chunks: Vec<String>,
    stream_order: Arc<Mutex<Vec<String>>>,
}

impl OpenRouterFakeHttpClient {
    fn completion(response: Value) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: response,
            completion_responses: Arc::new(Mutex::new(VecDeque::new())),
            stream_chunks: Vec::new(),
            stream_order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn completion_sequence(responses: Vec<Value>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: json!({}),
            completion_responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            stream_chunks: Vec::new(),
            stream_order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn stream(chunks: Vec<String>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: json!({}),
            completion_responses: Arc::new(Mutex::new(VecDeque::new())),
            stream_chunks: chunks,
            stream_order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<LlmHttpRequest> {
        self.calls.lock().unwrap().clone()
    }

    fn stream_order(&self) -> Vec<String> {
        self.stream_order.lock().unwrap().clone()
    }
}

impl LlmHttpClient for OpenRouterFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().unwrap().push(request.clone());
        if let Some(response) = self.completion_responses.lock().unwrap().pop_front() {
            return Ok(response);
        }
        Ok(self.completion_response.clone())
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        self.calls.lock().unwrap().push(request.clone());
        if !self.stream_chunks.is_empty() {
            return Ok(self.stream_chunks.clone());
        }
        let response = self
            .completion_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.completion_response.clone());
        let mut delta = response["choices"][0]["message"].clone();
        let finish_reason = response
            .pointer("/choices/0/finish_reason")
            .cloned()
            .unwrap_or_else(|| json!("stop"));
        if let Some(calls) = delta["tool_calls"].as_array_mut() {
            for (index, call) in calls.iter_mut().enumerate() {
                call["index"] = json!(index);
            }
        }
        Ok(vec![sse_data(
            json!({"choices":[{"delta":delta,"finish_reason":finish_reason}]}),
        )])
    }

    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut dyn FnMut(String) -> Result<std::ops::ControlFlow<()>>,
    ) -> Result<()> {
        let mut chunks = self.stream_text(request)?;
        // A completed round must stop before this malformed body tail.
        chunks.push("data: not-json-after-finish\n\n".to_string());
        for (index, chunk) in chunks.into_iter().enumerate() {
            self.stream_order
                .lock()
                .unwrap()
                .push(format!("tool-chunk:{index}"));
            if on_chunk(chunk)?.is_break() {
                return Ok(());
            }
        }
        Ok(())
    }

    fn stream_text_with_chunks(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut LlmTextChunkSink<'_>,
    ) -> Result<()> {
        self.calls.lock().unwrap().push(request.clone());
        for (index, chunk) in self.stream_chunks.iter().enumerate() {
            self.stream_order
                .lock()
                .unwrap()
                .push(format!("chunk:{index}"));
            on_chunk(chunk.clone())?;
        }
        Ok(())
    }
}

#[test]
fn openrouter_complete_posts_chat_completions_payload_and_parses_message_content() {
    let http_client = OpenRouterFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "assistant reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());

    let response = registry
        .complete("openrouter", &openrouter_request(false))
        .unwrap();

    assert_eq!(response.content, "assistant reply");
    assert_eq!(http_client.calls(), vec![expected_call(true)]);
}

#[test]
fn openrouter_request_can_select_provider_and_model() {
    let http_client = OpenRouterFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "selected reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.provider_id = Some("openrouter".to_string());
    request.model = Some("anthropic/claude-3.7-sonnet".to_string());

    let response = registry.complete_request(&request).unwrap();

    assert_eq!(response.content, "selected reply");
    assert_eq!(response.model, "anthropic/claude-3.7-sonnet");
    assert_eq!(
        http_client.calls()[0].payload["model"],
        json!("anthropic/claude-3.7-sonnet")
    );
}

#[test]
fn openrouter_applies_requested_reasoning_effort_without_capping_output() {
    let http_client = OpenRouterFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "quick answer" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.options = serde_json::from_value(json!({"reasoning_effort":"low"})).unwrap();

    assert_eq!(
        registry.complete("openrouter", &request).unwrap().content,
        "quick answer"
    );
    let payload = &http_client.calls()[0].payload;
    assert_eq!(payload["reasoning"], json!({"effort":"low"}));
    assert!(payload.get("max_tokens").is_none());
}

#[test]
fn openrouter_payload_maps_image_inputs_to_multimodal_message_content() {
    let http_client = OpenRouterFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "vision reply" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "image-1".to_string(),
        kind: LlmModalityInputKind::ImageSnapshot,
        media_type: "image/png".to_string(),
        bytes: vec![137, 80, 78, 71],
        metadata: json!({}),
    }];

    registry.complete("openrouter", &request).unwrap();

    let payload = http_client.calls()[0].payload.clone();
    assert_eq!(
        payload["messages"][0]["content"][0],
        json!({ "type": "text", "text": "hello" })
    );
    assert_eq!(
        payload["messages"][0]["content"][1]["type"],
        json!("image_url")
    );
    assert_eq!(
        payload["messages"][0]["content"][1]["image_url"]["url"],
        json!("data:image/png;base64,iVBORw==")
    );
}

#[test]
fn openrouter_rejects_live_audio_and_screen_frame_inputs() {
    let audio_error = openrouter_modality_error(LlmModalityInputKind::LiveAudioChunk);
    let frame_error = openrouter_modality_error(LlmModalityInputKind::ScreenFrame);

    assert!(audio_error.contains("live_audio_input is unsupported"));
    assert!(frame_error.contains("screen_frame_broadcast_input is unsupported"));
}

#[test]
fn openrouter_complete_rejects_generic_remote_payload() {
    let http_client = OpenRouterFakeHttpClient::completion(json!({
        "content": "generic remote response"
    }));
    let registry = registry_with_client(http_client.clone());

    let error = registry
        .complete("openrouter", &openrouter_request(false))
        .unwrap_err()
        .to_string();

    assert!(error.contains("OpenRouter content text"));
    assert_eq!(http_client.calls(), vec![expected_call(true)]);
}

#[test]
fn openrouter_stream_parses_sse_deltas_and_done() {
    let http_client = OpenRouterFakeHttpClient::stream(vec![
        stream_delta("hel"),
        stream_delta("lo"),
        "data: [DONE]\n\n".to_string(),
        stream_delta(" ignored"),
    ]);
    let registry = registry_with_client(http_client.clone());

    let events = registry
        .stream(
            "openrouter",
            &openrouter_request(true),
            StreamControl::unbounded(),
        )
        .unwrap();

    assert_eq!(events, completed_stream_events());
    assert_eq!(http_client.calls(), vec![expected_call(true)]);
}

#[test]
fn openrouter_stream_cancellation_emits_cancelled_event() {
    let http_client = OpenRouterFakeHttpClient::stream(vec![
        stream_delta("hel"),
        stream_delta("lo"),
        "data: [DONE]\n\n".to_string(),
    ]);
    let registry = registry_with_client(http_client);

    let events = registry
        .stream(
            "openrouter",
            &openrouter_request(true),
            StreamControl::cancel_after(1),
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "hel".to_string()
            },
            LlmStreamEvent::Cancelled,
        ]
    );
}

#[test]
fn openrouter_stream_with_events_emits_during_chunk_callbacks() {
    let http_client = OpenRouterFakeHttpClient::stream(vec![
        stream_delta("hel"),
        stream_delta("lo"),
        "data: [DONE]\n\n".to_string(),
    ]);
    let registry = registry_with_client(http_client.clone());
    let mut events = Vec::new();
    let order = http_client.stream_order.clone();

    registry
        .stream_with_events(
            "openrouter",
            &openrouter_request(true),
            StreamControl::unbounded(),
            &mut |event| {
                order.lock().unwrap().push(format!("event:{event:?}"));
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(events, completed_stream_events());
    assert_eq!(
        http_client.stream_order(),
        vec![
            "chunk:0",
            "event:ContentDelta { text: \"hel\" }",
            "chunk:1",
            "event:ContentDelta { text: \"lo\" }",
            "chunk:2",
            "event:Complete",
        ]
    );
}

#[test]
fn openrouter_stream_rejects_malformed_sse_delta_content() {
    let http_client = OpenRouterFakeHttpClient::stream(vec![sse_data(json!({
        "choices": [{ "delta": { "content": 42 } }]
    }))]);
    let registry = registry_with_client(http_client);

    let error = registry
        .stream(
            "openrouter",
            &openrouter_request(true),
            StreamControl::unbounded(),
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("OpenRouter content text"));
}

#[test]
fn openrouter_mcp_tool_calls_are_executed_through_scoped_server() {
    let mcp = FakeMcpServer::spawn(1);
    let http_client = OpenRouterFakeHttpClient::completion_sequence(vec![
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "builtin_assistant__assistant_respond",
                            "arguments": "{\"content\":\"hi from tool\"}"
                        }
                    }]
                }
            }]
        }),
        json!({
            "choices": [{ "message": { "content": "final answer" } }]
        }),
    ]);
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.options = serde_json::from_value(json!({"reasoning_effort":"low"})).unwrap();
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".to_string(),
        url: format!("{}/mcp/sse/builtin.assistant/session-a", mcp.url()),
    }];

    let response = registry.complete("openrouter", &request).unwrap();
    let calls = http_client.calls();

    assert_eq!(response.content, "final answer");
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|call| call.payload["reasoning"] == json!({"effort":"low"}))
    );
    assert_eq!(
        calls[0].payload["tools"][0]["function"]["name"],
        json!("builtin_assistant__assistant_respond")
    );
    assert_eq!(calls[0].payload["tool_choice"], json!("auto"));
    assert_eq!(
        calls[1].payload["messages"][1]["tool_calls"][0]["function"]["name"],
        json!("builtin_assistant__assistant_respond")
    );
    assert_eq!(
        mcp.tool_calls(),
        vec!["builtin_assistant__assistant_respond"]
    );
    assert_eq!(
        mcp.paths(),
        vec![
            "/mcp/messages/builtin.assistant/session-a",
            "/mcp/messages/builtin.assistant/session-a"
        ]
    );
}

#[test]
fn openrouter_completes_after_nine_tool_rounds_with_tools_enabled_throughout() {
    let tool_rounds = 9;
    let mcp = FakeMcpServer::spawn(tool_rounds);
    let mut responses = (1..=tool_rounds)
        .map(|round| {
            json!({"choices":[{"message":{"role":"assistant","content":null,
                "tool_calls":[{"id":format!("call-{round}"),"type":"function",
                    "function":{"name":"builtin_assistant__assistant_respond",
                        "arguments":"{\"content\":\"segment\"}"}}]}}]})
        })
        .collect::<Vec<_>>();
    responses.push(json!({
        "choices": [{"message": {"content": "Canvas is ready."}}]
    }));
    let http_client = OpenRouterFakeHttpClient::completion_sequence(responses);
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".to_string(),
        url: format!("{}/mcp/sse/builtin.assistant/session-nine", mcp.url()),
    }];

    let response = registry.complete("openrouter", &request).unwrap();
    let calls = http_client.calls();

    assert_eq!(response.content, "Canvas is ready.");
    assert_eq!(calls.len(), tool_rounds + 1);
    assert!(calls.iter().all(|call| {
        call.payload["stream"] == true
            && call.payload["tools"].is_array()
            && call.payload["tool_choice"] == "auto"
    }));
    assert_eq!(mcp.tool_calls().len(), tool_rounds);
    assert_eq!(http_client.stream_order().len(), tool_rounds + 1);
}

#[test]
fn openrouter_tool_round_exhaustion_has_no_extra_request_or_tool_call() {
    let tool_rounds = 32;
    let mcp = FakeMcpServer::spawn(tool_rounds);
    let responses = (1..=tool_rounds)
        .map(|round| {
            json!({"choices":[{"message":{"role":"assistant","content":null,
                "tool_calls":[{"id":format!("call-{round}"),"type":"function",
                    "function":{"name":"builtin_assistant__assistant_respond",
                        "arguments":"{\"content\":\"segment\"}"}}]}}]})
        })
        .collect();
    let http_client = OpenRouterFakeHttpClient::completion_sequence(responses);
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".to_string(),
        url: format!("{}/mcp/sse/builtin.assistant/session-exhausted", mcp.url()),
    }];

    let error = registry
        .complete("openrouter", &request)
        .expect_err("an unfinished 32nd tool round must fail clearly");
    let calls = http_client.calls();

    assert!(matches!(
        error,
        NeuralError::ToolRoundsExhausted { ref provider_id } if provider_id == "openrouter"
    ));
    assert_eq!(calls.len(), tool_rounds);
    assert!(calls.iter().all(|call| {
        call.payload["stream"] == true
            && call.payload["tools"].is_array()
            && call.payload["tool_choice"] == "auto"
    }));
    assert_eq!(mcp.tool_calls().len(), tool_rounds);
    assert_eq!(http_client.stream_order().len(), tool_rounds);
}

#[test]
fn openrouter_length_finish_reason_fails_without_an_extra_completion() {
    let mcp = FakeMcpServer::spawn(0);
    let http_client = OpenRouterFakeHttpClient::completion_sequence(vec![json!({
        "choices": [{
            "message": {"role": "assistant", "content": null},
            "finish_reason": "length"
        }]
    })]);
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".to_string(),
        url: format!("{}/mcp/sse/builtin.assistant/session-length", mcp.url()),
    }];

    let error = registry
        .complete("openrouter", &request)
        .expect_err("truncated output must not become an empty success");

    assert!(
        matches!(
            error,
            NeuralError::ProviderFailed { ref provider_id, ref message }
                if provider_id == "openrouter"
                    && message.to_ascii_lowercase().contains("output token limit")
        ),
        "expected a concise output-limit failure, got {error:?}"
    );
    assert_eq!(http_client.calls().len(), 1);
    assert!(http_client.calls()[0].payload["tools"].is_array());
    assert!(mcp.tool_calls().is_empty());
}

fn registry_with_client(http_client: OpenRouterFakeHttpClient) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(vec![openrouter_config()], Arc::new(http_client)).unwrap()
}

fn openrouter_config() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "openrouter".to_string(),
        kind: LlmProviderKind::OpenRouter,
        model: "openrouter/model".to_string(),
        endpoint: Some("https://openrouter.test/api/v1".to_string()),
        credential: Some("test-token".to_string()),
        completion_concurrency: None,
        spawn: None,
    }
}

fn openrouter_request(stream: bool) -> LlmRequest {
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

struct FakeMcpServer {
    url: String,
    calls: Arc<Mutex<Vec<String>>>,
    paths: Arc<Mutex<Vec<String>>>,
    _thread: JoinHandle<()>,
}

impl FakeMcpServer {
    fn spawn(tool_call_count: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let thread_calls = Arc::clone(&calls);
        let thread_paths = Arc::clone(&paths);
        let handle = std::thread::spawn(move || {
            for _ in 0..=tool_call_count {
                let (stream, _) = listener.accept().unwrap();
                handle_mcp_connection(stream, &thread_calls, &thread_paths);
            }
        });
        Self {
            url,
            calls,
            paths,
            _thread: handle,
        }
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn tool_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

fn handle_mcp_connection(
    mut stream: TcpStream,
    calls: &Arc<Mutex<Vec<String>>>,
    paths: &Arc<Mutex<Vec<String>>>,
) {
    let mut buffer = [0; 8192];
    let len = stream.read(&mut buffer).unwrap();
    let request = String::from_utf8_lossy(&buffer[..len]);
    paths
        .lock()
        .unwrap()
        .push(request_path(&request).to_string());
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("{}");
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let response = if payload["method"] == json!("tools/list") {
        mcp_tools_response()
    } else {
        let name = payload["params"]["name"].as_str().unwrap_or_default();
        calls.lock().unwrap().push(name.to_string());
        json!({ "jsonrpc": "2.0", "id": payload["id"].clone(), "result": { "ok": true } })
    };
    write_http_json(&mut stream, response);
}

fn request_path(request: &str) -> &str {
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
}

fn mcp_tools_response() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [{
                "name": "builtin_assistant__assistant_respond",
                "description": "Respond to the user",
                "inputSchema": {
                    "type": "object",
                    "properties": { "content": { "type": "string" } },
                    "required": ["content"]
                }
            }]
        }
    })
}

fn write_http_json(stream: &mut TcpStream, value: Value) {
    let body = value.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn openrouter_modality_error(kind: LlmModalityInputKind) -> String {
    let http_client = OpenRouterFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "ignored" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = openrouter_request(false);
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "modality-1".to_string(),
        kind,
        media_type: "application/octet-stream".to_string(),
        bytes: vec![1],
        metadata: json!({}),
    }];

    let error = registry
        .complete("openrouter", &request)
        .unwrap_err()
        .to_string();

    assert!(http_client.calls().is_empty());
    error
}

fn expected_call(stream: bool) -> LlmHttpRequest {
    LlmHttpRequest {
        timeout: None,
        endpoint: "https://openrouter.test/api/v1/chat/completions".to_string(),
        credential: "test-token".to_string(),
        headers: BTreeMap::new(),
        payload: json!({
            "model": "openrouter/model",
            "stream": stream,
            "messages": [{ "role": "user", "content": "hello" }]
        }),
    }
}

fn completed_stream_events() -> Vec<LlmStreamEvent> {
    vec![
        LlmStreamEvent::ContentDelta {
            text: "hel".to_string(),
        },
        LlmStreamEvent::ContentDelta {
            text: "lo".to_string(),
        },
        LlmStreamEvent::Complete,
    ]
}

fn stream_delta(text: &str) -> String {
    sse_data(json!({ "choices": [{ "delta": { "content": text } }] }))
}

fn sse_data(value: Value) -> String {
    format!("data: {value}\n\n")
}
