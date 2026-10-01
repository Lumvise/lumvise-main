#![allow(dead_code)]

use lumvise_neural_core::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmTextChunkSink,
};
use lumvise_neural_core::llm_providers::{
    LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmStreamEvent,
};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

// Tool name advertised by FakeMcpServer. Reused across tool-bridge proof tests so the
// discovery payload, tool_calls response, and recorded MCP calls stay in sync.
pub const CEREBRAS_TEST_TOOL_NAME: &str = "cerebras_tools__lookup";

#[derive(Clone)]
pub struct CerebrasFakeHttpClient {
    calls: Arc<Mutex<Vec<LlmHttpRequest>>>,
    completion_response: Value,
    completion_responses: Arc<Mutex<VecDeque<Value>>>,
    stream_chunks: Vec<String>,
}

impl CerebrasFakeHttpClient {
    pub fn completion(response: Value) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: response,
            completion_responses: Arc::new(Mutex::new(VecDeque::new())),
            stream_chunks: Vec::new(),
        }
    }

    pub fn completion_sequence(responses: Vec<Value>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: json!({}),
            completion_responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            stream_chunks: Vec::new(),
        }
    }

    pub fn stream(chunks: Vec<String>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            completion_response: json!({}),
            completion_responses: Arc::new(Mutex::new(VecDeque::new())),
            stream_chunks: chunks,
        }
    }

    pub fn calls(&self) -> Vec<LlmHttpRequest> {
        self.calls.lock().unwrap().clone()
    }
}

impl LlmHttpClient for CerebrasFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().unwrap().push(request.clone());
        if let Some(response) = self.completion_responses.lock().unwrap().pop_front() {
            return Ok(response);
        }
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

pub fn registry_with_client(http_client: CerebrasFakeHttpClient) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(vec![cerebras_config()], Arc::new(http_client)).unwrap()
}

pub fn registry_with_model(
    http_client: CerebrasFakeHttpClient,
    model: &str,
) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(
        vec![cerebras_config_with_model(model)],
        Arc::new(http_client),
    )
    .unwrap()
}

pub fn cerebras_config() -> LlmProviderConfig {
    cerebras_config_with_model("gemma-4-31b")
}

fn cerebras_config_with_model(model: &str) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "cerebras".into(),
        kind: LlmProviderKind::Cerebras,
        model: model.into(),
        endpoint: Some("https://api.cerebras.ai/v1".into()),
        credential: Some("test-token".into()),
        completion_concurrency: None,
        spawn: None,
    }
}

pub fn cerebras_request(stream: bool) -> LlmRequest {
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

pub fn cerebras_image_request(model: &str) -> LlmRequest {
    let mut request = cerebras_request(false);
    request.messages[0].content = "describe this".to_string();
    request.model = Some(model.to_string());
    request.modality_inputs = vec![image_modality_input()];
    request
}

fn image_modality_input() -> LlmModalityInput {
    LlmModalityInput {
        input_id: "image-1".to_string(),
        kind: LlmModalityInputKind::ImageSnapshot,
        media_type: "image/png".to_string(),
        bytes: vec![137, 80, 78, 71],
        metadata: json!({}),
    }
}

pub fn cerebras_modality_error(kind: LlmModalityInputKind) -> String {
    let http_client = CerebrasFakeHttpClient::completion(json!({
        "choices": [{ "message": { "content": "ignored" } }]
    }));
    let registry = registry_with_client(http_client.clone());
    let mut request = cerebras_request(false);
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "modality-1".to_string(),
        kind,
        media_type: "application/octet-stream".to_string(),
        bytes: vec![1],
        metadata: json!({}),
    }];

    let error = registry
        .complete("cerebras", &request)
        .unwrap_err()
        .to_string();
    assert!(http_client.calls().is_empty());
    error
}

pub fn expected_call(stream: bool) -> LlmHttpRequest {
    LlmHttpRequest {
        timeout: None,
        endpoint: "https://api.cerebras.ai/v1/chat/completions".to_string(),
        credential: "test-token".to_string(),
        headers: BTreeMap::new(),
        payload: json!({
            "model": "gemma-4-31b",
            "stream": stream,
            "messages": [{ "role": "user", "content": "hello" }]
        }),
    }
}

pub fn completed_stream_events() -> Vec<LlmStreamEvent> {
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

pub fn stream_delta(text: &str) -> String {
    sse_data(json!({ "choices": [{ "delta": { "content": text } }] }))
}

pub fn sse_data(value: Value) -> String {
    format!("data: {value}\n\n")
}

// Local MCP server used to prove the OpenAI-compatible function-tool bridge without
// live Cerebras credentials or external network. Mirrors the OpenRouter test seam:
// reqwest-blocking discovery (tools/list) and dispatch (tools/call) hit this loopback
// server, while the Cerebras Chat Completions calls hit CerebrasFakeHttpClient.
pub struct FakeMcpServer {
    url: String,
    calls: Arc<Mutex<Vec<String>>>,
    paths: Arc<Mutex<Vec<String>>>,
    _thread: JoinHandle<()>,
}

impl FakeMcpServer {
    pub fn spawn(connections: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let thread_calls = Arc::clone(&calls);
        let thread_paths = Arc::clone(&paths);
        let handle = std::thread::spawn(move || {
            for _ in 0..connections {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(_) => break,
                };
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

    pub fn url(&self) -> String {
        self.url.clone()
    }

    pub fn tool_calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn paths(&self) -> Vec<String> {
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
                "name": CEREBRAS_TEST_TOOL_NAME,
                "description": "Cerebras proof-of-bridge lookup tool",
                "inputSchema": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
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
