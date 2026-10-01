use lumvise_neural_core::llm_providers::adapter::{LlmAdapterPreferences, LlmTransportPreference};
use lumvise_neural_core::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmTextChunkSink,
};
use lumvise_neural_core::llm_providers::{
    LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmResponseFormat,
    LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{
    LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, NeuralError, Result,
};
use lumvise_resource_routing::InvocationControl;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
struct DirectApiFakeHttpClient {
    calls: Arc<Mutex<Vec<LlmHttpRequest>>>,
    responses: Arc<Mutex<VecDeque<Value>>>,
    stream_chunks: Arc<Mutex<VecDeque<Vec<String>>>>,
}

struct TimeoutLikeHttpClient;

impl LlmHttpClient for TimeoutLikeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        Err(NeuralError::ProviderFailed {
            provider_id: request.endpoint.clone(),
            message: "timeout while waiting for direct API".to_string(),
        })
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        Err(NeuralError::ProviderFailed {
            provider_id: request.endpoint.clone(),
            message: "timeout while waiting for direct API stream".to_string(),
        })
    }
}

impl DirectApiFakeHttpClient {
    fn new(responses: Vec<Value>, stream_chunks: Vec<Vec<String>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            stream_chunks: Arc::new(Mutex::new(VecDeque::from(stream_chunks))),
        }
    }

    fn calls(&self) -> Vec<LlmHttpRequest> {
        self.calls.lock().clone()
    }
}

impl LlmHttpClient for DirectApiFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().push(request.clone());
        Ok(self.responses.lock().pop_front().unwrap_or(json!({})))
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        self.calls.lock().push(request.clone());
        Ok(self.stream_chunks.lock().pop_front().unwrap_or_default())
    }

    fn stream_text_with_chunks(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut LlmTextChunkSink<'_>,
    ) -> Result<()> {
        self.calls.lock().push(request.clone());
        for chunk in self.stream_text_without_recording() {
            on_chunk(chunk)?;
        }
        Ok(())
    }
}

impl DirectApiFakeHttpClient {
    fn stream_text_without_recording(&self) -> Vec<String> {
        self.stream_chunks.lock().pop_front().unwrap_or_default()
    }
}

#[test]
fn claude_direct_api_posts_anthropic_messages_payload_and_streams_events() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({ "content": [{ "type": "text", "text": "claude final" }] })],
        vec![vec![
            sse(json!({ "type": "content_block_delta", "delta": { "text": "claude " } })),
            sse(json!({ "type": "content_block_delta", "delta": { "text": "stream" } })),
            sse(json!({ "type": "message_stop" })),
        ]],
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());

    let response = registry.complete("claude", &image_request()).unwrap();
    let events = registry
        .stream("claude", &text_request(), StreamControl::unbounded())
        .unwrap();

    assert_eq!(response.content, "claude final");
    assert_eq!(events, text_events("claude ", "stream"));
    let calls = http_client.calls();
    assert_eq!(calls[0].endpoint, "https://provider.test/v1/messages");
    assert_eq!(calls[0].headers["anthropic-version"], "2023-06-01");
    assert_eq!(calls[0].headers["x-api-key"], "token");
    assert_eq!(
        calls[0].payload["messages"][0]["content"][1]["type"],
        "image"
    );
    assert_eq!(calls[1].payload["stream"], json!(true));
}

#[test]
fn gemini_direct_api_posts_generate_content_payload_and_streams_events() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({
            "candidates": [{ "content": { "parts": [{ "text": "gemini final" }] } }]
        })],
        vec![vec![
            sse(json!({ "candidates": [{ "content": { "parts": [{ "text": "gemini " }] } }] })),
            sse(json!({ "candidates": [{ "content": { "parts": [{ "text": "stream" }] } }] })),
        ]],
    );
    let registry = registry("gemini", LlmProviderKind::Gemini, http_client.clone());

    let response = registry.complete("gemini", &image_request()).unwrap();
    let events = registry
        .stream("gemini", &text_request(), StreamControl::unbounded())
        .unwrap();

    assert_eq!(response.content, "gemini final");
    assert_eq!(events, text_events("gemini ", "stream"));
    let calls = http_client.calls();
    assert_eq!(
        calls[0].endpoint,
        "https://provider.test/models/model:generateContent"
    );
    assert_eq!(calls[0].headers["x-goog-api-key"], "token");
    assert_eq!(
        calls[0].payload["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        calls[1].endpoint,
        "https://provider.test/models/model:streamGenerateContent"
    );
}

#[test]
fn codex_openai_direct_api_posts_responses_payload_and_emits_session() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({ "id": "resp-1", "output_text": "openai final" })],
        vec![vec![
            sse(json!({ "type": "response.output_text.delta", "delta": "openai " })),
            sse(json!({ "type": "response.output_text.delta", "delta": "stream" })),
            sse(json!({ "type": "response.completed", "response": { "id": "resp-2" } })),
        ]],
    );
    let registry = registry("codex", LlmProviderKind::Codex, http_client.clone());
    let mut request = image_request();
    request.provider_session_id = Some("resp-existing".to_string());

    let response = registry.complete("codex", &request).unwrap();
    let events = registry
        .stream("codex", &text_request(), StreamControl::unbounded())
        .unwrap();

    assert_eq!(response.content, "openai final");
    assert_eq!(response.metadata["provider_session_id"], json!("resp-1"));
    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "openai ".to_string()
            },
            LlmStreamEvent::ContentDelta {
                text: "stream".to_string()
            },
            LlmStreamEvent::Session {
                provider_session_id: "resp-2".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
    let calls = http_client.calls();
    assert_eq!(calls[0].endpoint, "https://provider.test/v1/responses");
    assert_eq!(calls[0].payload["previous_response_id"], "resp-existing");
    assert_eq!(
        calls[0].payload["input"][0]["content"][1]["type"],
        "input_image"
    );
}

#[test]
fn malformed_direct_stream_fails_safely() {
    let http_client =
        DirectApiFakeHttpClient::new(Vec::new(), vec![vec!["data: not-json\n\n".to_string()]]);
    let registry = registry("claude", LlmProviderKind::Claude, http_client);

    let error = registry
        .stream("claude", &text_request(), StreamControl::unbounded())
        .unwrap_err()
        .to_string();

    assert!(error.contains("Claude SSE JSON data"));
}

// LlmRequestOptions applied by the direct-API transports.

#[test]
fn claude_direct_api_applies_max_tokens_temperature_and_json_system_prompt() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({ "content": [{ "type": "text", "text": "{\"ok\":1}" }] })],
        Vec::new(),
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());
    let mut request = text_request();
    request.stream = false;
    request.options.max_output_tokens = Some(777);
    request.options.temperature = Some(0.9);
    request.options.response_format = Some(LlmResponseFormat::JsonSchema {
        name: "answer".to_string(),
        schema: json!({ "type": "object", "properties": {} }),
        strict: false,
    });

    let response = registry.complete("claude", &request).unwrap();

    assert_eq!(response.content, "{\"ok\":1}");
    let payload = &http_client.calls()[0].payload;
    assert_eq!(payload["max_tokens"], json!(777));
    let temperature = payload["temperature"].as_f64().unwrap();
    assert!(
        (temperature - 0.9).abs() < 0.001,
        "temperature was {temperature}"
    );
    let system = payload["system"].as_str().unwrap();
    assert!(system.contains("Reply with only one JSON object"));
    assert!(system.contains("JSON Schema"));
}

#[test]
fn claude_direct_api_uses_default_max_tokens_without_options() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({ "content": [{ "type": "text", "text": "claude" }] })],
        Vec::new(),
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());
    let mut request = text_request();
    request.stream = false;

    registry.complete("claude", &request).unwrap();

    let payload = &http_client.calls()[0].payload;
    assert_eq!(payload["max_tokens"], json!(16_384));
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("system").is_none());
}

#[test]
fn gemini_direct_api_applies_generation_config_from_options() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({
            "candidates": [{ "content": { "parts": [{ "text": "gemini" }] } }]
        })],
        Vec::new(),
    );
    let registry = registry("gemini", LlmProviderKind::Gemini, http_client.clone());
    let mut request = text_request();
    request.stream = false;
    request.options.max_output_tokens = Some(256);
    request.options.temperature = Some(0.1);
    request.options.response_format = Some(LlmResponseFormat::JsonObject);

    registry.complete("gemini", &request).unwrap();

    let config = &http_client.calls()[0].payload["generationConfig"];
    assert_eq!(config["maxOutputTokens"], json!(256));
    let temperature = config["temperature"].as_f64().unwrap();
    assert!(
        (temperature - 0.1).abs() < 0.001,
        "temperature was {temperature}"
    );
    assert_eq!(config["responseMimeType"], json!("application/json"));
    assert!(config.get("responseSchema").is_none());
}

#[test]
fn gemini_direct_api_maps_plain_object_schema_to_response_schema() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({
            "candidates": [{ "content": { "parts": [{ "text": "gemini" }] } }]
        })],
        Vec::new(),
    );
    let registry = registry("gemini", LlmProviderKind::Gemini, http_client.clone());
    let mut request = text_request();
    request.stream = false;
    request.options.response_format = Some(LlmResponseFormat::JsonSchema {
        name: "answer".to_string(),
        schema: json!({
            "type": "object",
            "properties": { "ok": { "type": "boolean" } },
            "required": ["ok"]
        }),
        strict: true,
    });

    registry.complete("gemini", &request).unwrap();

    let config = &http_client.calls()[0].payload["generationConfig"];
    assert_eq!(config["responseMimeType"], json!("application/json"));
    assert_eq!(
        config["responseSchema"]["properties"]["ok"]["type"],
        json!("boolean")
    );
}

#[test]
fn direct_api_controlled_completion_bounds_http_timeout() {
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({ "content": [{ "type": "text", "text": "claude controlled" }] })],
        Vec::new(),
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());
    let mut request = text_request();
    request.stream = false;
    let control = InvocationControl::with_deadline(Duration::from_secs(45));

    let response = registry
        .provider_handle("claude")
        .unwrap()
        .complete_controlled(&request, &control)
        .unwrap();

    assert_eq!(response.content, "claude controlled");
    let timeout = http_client.calls()[0]
        .timeout
        .expect("direct API call carries the caller deadline");
    assert!(timeout <= Duration::from_secs(45) && timeout > Duration::ZERO);
}

#[test]
fn direct_api_timeout_like_provider_error_surfaces_without_fallback() {
    let registry = LlmProviderRegistry::from_configs_with_preferences(
        vec![LlmProviderConfig {
            provider_id: "claude".to_string(),
            kind: LlmProviderKind::Claude,
            model: "model".to_string(),
            endpoint: Some("https://provider.test".to_string()),
            credential: Some("token".to_string()),
            completion_concurrency: None,
            spawn: None,
        }],
        Arc::new(TimeoutLikeHttpClient),
        LlmAdapterPreferences {
            transport_preference: LlmTransportPreference::PreferDirectApi,
            allow_fallbacks: false,
        },
    )
    .unwrap();

    let error = registry
        .complete("claude", &text_request())
        .unwrap_err()
        .to_string();

    assert!(error.contains("timeout while waiting for direct API"));
}

#[test]
fn claude_direct_api_executes_mcp_tool_call() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "content": [{ "type": "tool_use", "id": "toolu-1", "name": "assistant_respond", "input": { "text": "hi" } }] }),
            json!({ "content": [{ "type": "text", "text": "tool done" }] }),
        ],
        Vec::new(),
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());

    let response = registry
        .complete("claude", &mcp_request(mcp.url()))
        .unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    assert!(http_client.calls()[0].payload["tools"].is_array());
}

#[test]
fn gemini_direct_api_executes_mcp_tool_call() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "candidates": [{ "content": { "parts": [{ "functionCall": { "name": "assistant_respond", "args": { "text": "hi" } } }] } }] }),
            json!({ "candidates": [{ "content": { "parts": [{ "text": "tool done" }] } }] }),
        ],
        Vec::new(),
    );
    let registry = registry("gemini", LlmProviderKind::Gemini, http_client.clone());

    let response = registry
        .complete("gemini", &mcp_request(mcp.url()))
        .unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    assert!(http_client.calls()[0].payload["tools"].is_array());
}

#[test]
fn gemini_direct_api_feeds_back_schema_validation_and_retries_with_corrected_arguments() {
    let input_schema = json!({
        "type": "object",
        "required": ["color"],
        "properties": {
            "color": { "type": "string", "enum": ["red", "blue"] }
        },
        "additionalProperties": false
    });
    let mcp = FakeMcpServer::spawn_with_tool_schema(2, input_schema.clone());
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "candidates": [{ "content": { "parts": [{
                "functionCall": { "name": "assistant_respond", "args": { "color": "purple" } }
            }] } }] }),
            json!({ "candidates": [{ "content": { "parts": [{
                "functionCall": { "name": "assistant_respond", "args": { "color": "blue" } }
            }] } }] }),
            json!({ "candidates": [{ "content": { "parts": [{ "text": "tool done" }] } }] }),
        ],
        Vec::new(),
    );
    let registry = registry("gemini", LlmProviderKind::Gemini, http_client.clone());

    let response = registry
        .complete("gemini", &mcp_request(mcp.url()))
        .unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    let calls = http_client.calls();
    assert_eq!(
        calls[0].payload["tools"][0]["functionDeclarations"][0]["parameters"],
        input_schema
    );
    let feedback = calls[1].payload["contents"][2]["parts"][0]["functionResponse"]["response"]
        ["error"]["message"]
        .as_str()
        .unwrap();
    assert!(
        feedback.contains("color")
            && feedback.contains("red")
            && feedback.contains("blue")
            && feedback.contains("retry")
    );
    assert_eq!(
        mcp.payloads()[1]["params"]["arguments"],
        json!({ "color": "blue" })
    );
}

#[test]
fn codex_openai_direct_api_executes_mcp_tool_call() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "output": [{ "type": "function_call", "call_id": "call-1", "name": "assistant_respond", "arguments": "{\"text\":\"hi\"}" }] }),
            json!({ "id": "resp-3", "output_text": "tool done" }),
        ],
        Vec::new(),
    );
    let registry = registry("codex", LlmProviderKind::Codex, http_client.clone());

    let response = registry.complete("codex", &mcp_request(mcp.url())).unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    assert!(http_client.calls()[0].payload["tools"].is_array());
}

// F-009: a model can emit a tool call in Gemma's native (non-JSON) text
// serialization instead of the provider's structured tool-call response;
// every direct-API provider recovers and dispatches it identically to a
// real structured call, and never delivers an unresolvable attempt to the
// caller as if it were an ordinary answer.

#[test]
fn claude_direct_api_recovers_a_gemma_native_tool_call() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "content": [{ "type": "text", "text": "call:assistant_respond{text:hi}" }] }),
            json!({ "content": [{ "type": "text", "text": "tool done" }] }),
        ],
        Vec::new(),
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());

    let response = registry
        .complete("claude", &mcp_request(mcp.url()))
        .unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    assert_eq!(
        mcp.payloads()[1]["params"]["arguments"],
        json!({ "text": "hi" })
    );
}

#[test]
fn claude_direct_api_never_delivers_a_malformed_native_tool_call_attempt() {
    let mcp = FakeMcpServer::spawn(1);
    let http_client = DirectApiFakeHttpClient::new(
        vec![json!({ "content": [{ "type": "text", "text": "call:assistant_respond{text:" }] })],
        Vec::new(),
    );
    let registry = registry("claude", LlmProviderKind::Claude, http_client.clone());

    let error = registry
        .complete("claude", &mcp_request(mcp.url()))
        .unwrap_err();

    assert!(matches!(error, NeuralError::MalformedPayload { .. }));
    assert_eq!(mcp.methods(), vec!["tools/list"]);
}

#[test]
fn gemini_direct_api_recovers_a_gemma_native_tool_call() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "candidates": [{ "content": { "parts": [{ "text": "call:assistant_respond{text:hi}" }] } }] }),
            json!({ "candidates": [{ "content": { "parts": [{ "text": "tool done" }] } }] }),
        ],
        Vec::new(),
    );
    let registry = registry("gemini", LlmProviderKind::Gemini, http_client.clone());

    let response = registry
        .complete("gemini", &mcp_request(mcp.url()))
        .unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    assert_eq!(
        mcp.payloads()[1]["params"]["arguments"],
        json!({ "text": "hi" })
    );
}

#[test]
fn codex_openai_direct_api_recovers_a_gemma_native_tool_call() {
    let mcp = FakeMcpServer::spawn(2);
    let http_client = DirectApiFakeHttpClient::new(
        vec![
            json!({ "output_text": "call:assistant_respond{text:hi}" }),
            json!({ "id": "resp-3", "output_text": "tool done" }),
        ],
        Vec::new(),
    );
    let registry = registry("codex", LlmProviderKind::Codex, http_client.clone());

    let response = registry.complete("codex", &mcp_request(mcp.url())).unwrap();

    assert_eq!(response.content, "tool done");
    assert_eq!(mcp.methods(), vec!["tools/list", "tools/call"]);
    assert_eq!(
        mcp.payloads()[1]["params"]["arguments"],
        json!({ "text": "hi" })
    );
}

fn registry(
    provider_id: &str,
    kind: LlmProviderKind,
    http_client: DirectApiFakeHttpClient,
) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs_with_preferences(
        vec![LlmProviderConfig {
            provider_id: provider_id.to_string(),
            kind,
            model: "model".to_string(),
            endpoint: Some("https://provider.test".to_string()),
            credential: Some("token".to_string()),
            completion_concurrency: None,
            spawn: None,
        }],
        Arc::new(http_client),
        LlmAdapterPreferences {
            transport_preference: LlmTransportPreference::PreferDirectApi,
            allow_fallbacks: false,
        },
    )
    .unwrap()
}

fn text_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream: true,
        provider_id: None,
        model: None,
        conversation_id: Some("conversation-a".to_string()),
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

fn image_request() -> LlmRequest {
    let mut request = text_request();
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "image-1".to_string(),
        kind: LlmModalityInputKind::ImageSnapshot,
        media_type: "image/png".to_string(),
        bytes: vec![137, 80, 78, 71],
        metadata: json!({}),
    }];
    request
}

fn mcp_request(server_url: String) -> LlmRequest {
    let mut request = text_request();
    request.mcp_servers = vec![lumvise_neural_core::llm_providers::LlmMcpServerConfig {
        name: "builtin.assistant".to_string(),
        url: format!("{server_url}/mcp/sse/builtin.assistant/session-a"),
    }];
    request
}

fn text_events(left: &str, right: &str) -> Vec<LlmStreamEvent> {
    vec![
        LlmStreamEvent::ContentDelta {
            text: left.to_string(),
        },
        LlmStreamEvent::ContentDelta {
            text: right.to_string(),
        },
        LlmStreamEvent::Complete,
    ]
}

fn sse(value: Value) -> String {
    format!("data: {value}\n\n")
}

struct FakeMcpServer {
    url: String,
    methods: Arc<Mutex<Vec<String>>>,
    payloads: Arc<Mutex<Vec<Value>>>,
    _thread: JoinHandle<()>,
}

impl FakeMcpServer {
    fn spawn(requests: usize) -> Self {
        Self::spawn_with_tool_schema(requests, json!({ "type": "object" }))
    }

    fn spawn_with_tool_schema(requests: usize, input_schema: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let methods = Arc::new(Mutex::new(Vec::new()));
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let thread_methods = methods.clone();
        let thread_payloads = payloads.clone();
        let handle = thread::spawn(move || {
            for _ in 0..requests {
                let (stream, _) = listener.accept().unwrap();
                handle_mcp_connection(stream, &thread_methods, &thread_payloads, &input_schema);
            }
        });
        Self {
            url,
            methods,
            payloads,
            _thread: handle,
        }
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().clone()
    }

    fn payloads(&self) -> Vec<Value> {
        self.payloads.lock().clone()
    }
}

fn handle_mcp_connection(
    mut stream: TcpStream,
    methods: &Arc<Mutex<Vec<String>>>,
    payloads: &Arc<Mutex<Vec<Value>>>,
    input_schema: &Value,
) {
    let body = read_http_body(&mut stream);
    let value: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let method = value["method"].as_str().unwrap_or("").to_string();
    methods.lock().push(method.clone());
    payloads.lock().push(value);
    let response = if method == "tools/list" {
        tools_list_response(input_schema)
    } else {
        json!({ "jsonrpc": "2.0", "id": 2, "result": { "content": "ok" } })
    };
    write_http_json(&mut stream, response);
}

fn tools_list_response(input_schema: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "tools": [{
                "name": "assistant_respond",
                "description": "respond",
                "inputSchema": input_schema
            }]
        }
    })
}

fn read_http_body(stream: &mut TcpStream) -> String {
    let mut buffer = [0_u8; 4096];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]);
    request.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

fn write_http_json(stream: &mut TcpStream, value: Value) {
    let body = value.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}
