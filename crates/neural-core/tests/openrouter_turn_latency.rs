use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{
    LlmMcpServerConfig, LlmMessage, LlmRequest, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, Result};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ops::ControlFlow;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

#[derive(Clone, Default)]
struct StreamedModel {
    rounds: Arc<Mutex<VecDeque<Vec<String>>>>,
    requests: Arc<Mutex<Vec<LlmHttpRequest>>>,
    cancelled: Arc<AtomicBool>,
    cancel_on_chunk: bool,
    read_tail: Arc<AtomicBool>,
}

impl StreamedModel {
    fn new(rounds: Vec<Vec<String>>) -> Self {
        Self {
            rounds: Arc::new(Mutex::new(rounds.into())),
            ..Self::default()
        }
    }

    fn registry(&self) -> LlmProviderRegistry {
        LlmProviderRegistry::from_configs(
            vec![LlmProviderConfig {
                provider_id: "openrouter".into(),
                kind: LlmProviderKind::OpenRouter,
                model: "test/model".into(),
                endpoint: Some("https://unused.test/api/v1".into()),
                credential: Some("fake".into()),
                completion_concurrency: None,
                spawn: None,
            }],
            Arc::new(self.clone()),
        )
        .unwrap()
    }
}

impl LlmHttpClient for StreamedModel {
    fn post_json(&self, _: &LlmHttpRequest) -> Result<Value> {
        panic!("tool turns must use incremental streaming")
    }
    fn stream_text(&self, _: &LlmHttpRequest) -> Result<Vec<String>> {
        panic!("tool turns must not buffer the full HTTP body")
    }
    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        sink: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    ) -> Result<()> {
        self.requests.lock().unwrap().push(request.clone());
        let chunks = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra model request");
        for chunk in chunks {
            if self.cancel_on_chunk {
                self.cancelled.store(true, Ordering::SeqCst);
            }
            if sink(chunk)?.is_break() {
                return Ok(());
            }
        }
        self.read_tail.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct ScopedAssistant {
    address: String,
    invocations: Arc<Mutex<Vec<Value>>>,
    discoveries: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ScopedAssistant {
    fn start(outcome: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let discoveries = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (calls, lists, stop) = (invocations.clone(), discoveries.clone(), stopped.clone());
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                serve_rpc(stream.unwrap(), &outcome, &calls, &lists);
            }
        });
        Self {
            address,
            invocations,
            discoveries,
            stopped,
            thread: Some(thread),
        }
    }

    fn request(&self, session: &str) -> LlmRequest {
        LlmRequest {
            options: Default::default(),
            messages: vec![LlmMessage {
                role: "user".into(),
                content: "Answer my question".into(),
            }],
            stream: true,
            provider_id: None,
            model: None,
            conversation_id: Some(session.into()),
            provider_session_id: None,
            modality_inputs: vec![],
            mcp_servers: vec![LlmMcpServerConfig {
                name: "assistant".into(),
                url: format!("http://{}/mcp/messages/{}", self.address, session),
            }],
        }
    }
}

impl Drop for ScopedAssistant {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn serve_rpc(
    mut stream: TcpStream,
    outcome: &Value,
    calls: &Mutex<Vec<Value>>,
    lists: &Mutex<Vec<String>>,
) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(&stream);
    let mut header = String::new();
    let mut size = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
            size = value.trim().parse().unwrap();
        }
        header.push_str(&line);
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body).unwrap();
    let rpc: Value = serde_json::from_slice(&body).unwrap();
    let result = if rpc["method"] == "tools/list" {
        lists
            .lock()
            .unwrap()
            .push(header.lines().next().unwrap().into());
        json!({"tools":[{"name":"assistant_respond","description":"Speak","inputSchema":{"type":"object","properties":{"content":{"type":"string"},"final":{"type":"boolean"}},"required":["content"]}}, {"name":"assistant_finish","description":"Close session","inputSchema":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}}]})
    } else {
        calls.lock().unwrap().push(rpc["params"].clone());
        outcome.clone()
    };
    let body = json!({"jsonrpc":"2.0","id":rpc["id"],"result":result}).to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
}

fn acknowledged(finalized: bool) -> Value {
    json!({"content":[{"type":"text","text":json!({"decision":"assistant_responded","state":{"app_response_finalized":finalized,"last_response":"Hello 🐼"}}).to_string()}]})
}

fn event(delta: Value, finish: Value) -> String {
    format!(
        "data: {}\r\n\r\n",
        json!({"choices":[{"delta":delta,"finish_reason":finish}]})
    )
}

fn speech_round() -> Vec<String> {
    vec![
        event(
            json!({"tool_calls":[{"index":0,"id":"call-1","function":{"name":"assistant_respond","arguments":"{\"content\":\"Hello "}}]}),
            Value::Null,
        ),
        event(
            json!({"tool_calls":[{"index":0,"function":{"arguments":"🐼\",\"final\":true}"}}]}),
            Value::Null,
        ),
        event(json!({}), json!("tool_calls")),
        "data: malformed unused tail\n\n".into(),
    ]
}

fn text_round() -> Vec<String> {
    vec![event(
        json!({"content":"Continue after tool result"}),
        json!("stop"),
    )]
}

#[test]
fn finalized_speech_uses_one_streamed_round_and_reuses_conversation_discovery() {
    let assistant = ScopedAssistant::start(acknowledged(true));
    let model = StreamedModel::new(vec![speech_round(), speech_round(), speech_round()]);
    let registry = model.registry();
    let request = assistant.request("session-a");
    for _ in 0..2 {
        assert_eq!(
            registry.complete("openrouter", &request).unwrap().content,
            "Hello 🐼"
        );
    }
    assert_eq!(assistant.discoveries.lock().unwrap().len(), 1);
    assert_eq!(assistant.invocations.lock().unwrap().len(), 2);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert!(!model.read_tail.load(Ordering::SeqCst));
    registry
        .complete("openrouter", &assistant.request("session-b"))
        .unwrap();
    assert_eq!(assistant.discoveries.lock().unwrap().len(), 2);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests[0].payload["session_id"], "session-a");
    assert_eq!(requests[1].payload["session_id"], "session-a");
    assert_eq!(requests[2].payload["session_id"], "session-b");
    assert!(
        requests
            .iter()
            .all(|request| request.payload["stream"] == true)
    );
    assert_eq!(
        assistant.invocations.lock().unwrap()[0]["arguments"]["content"],
        "Hello 🐼"
    );
}

#[test]
fn partial_or_failed_speech_requires_model_continuation_and_preserves_reasoning() {
    for outcome in [
        acknowledged(false),
        json!({"isError":true,"content":[{"type":"text","text":"speech rejected"}]}),
    ] {
        let assistant = ScopedAssistant::start(outcome);
        let mut chunks = speech_round();
        chunks.insert(0, event(json!({"reasoning_details":[{"index":0,"id":"r1","type":"reasoning.text","text":"First "}]}), Value::Null));
        chunks.insert(
            1,
            event(
                json!({"reasoning_details":[{"index":0,"text":"second"}]}),
                Value::Null,
            ),
        );
        let model = StreamedModel::new(vec![chunks, text_round()]);
        let registry = model.registry();
        assert_eq!(
            registry
                .complete("openrouter", &assistant.request("partial"))
                .unwrap()
                .content,
            "Continue after tool result"
        );
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].payload["session_id"], "partial");
        assert_eq!(
            requests[1].payload["messages"][1]["reasoning_details"],
            json!([{"index":0,"id":"r1","type":"reasoning.text","text":"First second"}])
        );
    }
}

#[test]
fn incomplete_or_malformed_streams_never_dispatch_tools_and_invalidate_discovery() {
    let mut truncated = speech_round();
    truncated.truncate(2); // Arguments are valid JSON, but the model has not finished the call.
    for chunks in [
        truncated,
        vec!["data: {broken}\n\n".into()],
        vec![event(
            json!({"tool_calls":[{"index":0,"id":"x","function":{"name":"assistant_respond","arguments":"{bad"}}]}),
            json!("tool_calls"),
        )],
    ] {
        let assistant = ScopedAssistant::start(acknowledged(true));
        let model = StreamedModel::new(vec![chunks, speech_round()]);
        let registry = model.registry();
        let request = assistant.request("recover");
        assert!(registry.complete("openrouter", &request).is_err());
        assert!(assistant.invocations.lock().unwrap().is_empty());
        registry.complete("openrouter", &request).unwrap();
        assert_eq!(assistant.discoveries.lock().unwrap().len(), 2);
        assert_eq!(assistant.invocations.lock().unwrap().len(), 1);
    }
}

#[test]
fn cancelled_stream_does_not_execute_complete_tool_arguments() {
    let assistant = ScopedAssistant::start(acknowledged(true));
    let mut model = StreamedModel::new(vec![speech_round()]);
    model.cancel_on_chunk = true;
    let registry = model.registry();
    let control = StreamControl::unbounded().with_cancel_flag(model.cancelled.clone());
    let events = registry
        .stream("openrouter", &assistant.request("cancel"), control)
        .unwrap();
    assert_eq!(events, vec![LlmStreamEvent::Cancelled]);
    assert!(assistant.invocations.lock().unwrap().is_empty());
}

#[test]
fn changing_scoped_route_rediscover_tools_even_with_same_conversation() {
    let assistant = ScopedAssistant::start(acknowledged(true));
    let model = StreamedModel::new(vec![speech_round(), speech_round()]);
    let registry = model.registry();
    let mut request = assistant.request("same-session");
    registry.complete("openrouter", &request).unwrap();
    request.mcp_servers[0].url.push_str("-new-generation");
    registry.complete("openrouter", &request).unwrap();
    assert_eq!(assistant.discoveries.lock().unwrap().len(), 2);
}

#[test]
fn acknowledged_session_close_does_not_request_or_speak_another_model_response() {
    let outcome = json!({"structuredContent":{"decision":"assistant_finished","state":{"phase":"completed"},"degraded":true}});
    let assistant = ScopedAssistant::start(outcome);
    let close = event(
        json!({"tool_calls":[{"index":0,"id":"close-1","function":{"name":"assistant_finish","arguments":"{\"summary\":\"Closed at user request\"}"}}]}),
        json!("tool_calls"),
    );
    let model = StreamedModel::new(vec![vec![close]]);
    let registry = model.registry();
    assert_eq!(
        registry
            .complete("openrouter", &assistant.request("close"))
            .unwrap()
            .content,
        ""
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert_eq!(
        assistant.invocations.lock().unwrap()[0]["name"],
        "assistant_finish"
    );
}

#[test]
fn interleaved_tool_deltas_keep_each_argument_and_execute_in_index_order() {
    let assistant = ScopedAssistant::start(acknowledged(true));
    let first = event(
        json!({"tool_calls":[
            {"index":1,"id":"b","function":{"name":"assistant_respond","arguments":"{\"content\":\"Second"}},
            {"index":0,"id":"a","function":{"name":"assistant_respond","arguments":"{\"content\":\"First"}}
        ]}),
        Value::Null,
    );
    let second = event(
        json!({"tool_calls":[
            {"index":0,"function":{"arguments":"\"}"}},
            {"index":1,"function":{"arguments":"\"}"}}
        ]}),
        json!("tool_calls"),
    );
    // Also split an SSE JSON frame across reads, independently of tool argument fragmentation.
    let chunks = vec![first[..15].into(), first[15..].into(), second];
    let model = StreamedModel::new(vec![chunks]);
    model
        .registry()
        .complete("openrouter", &assistant.request("interleaved"))
        .unwrap();
    let calls = assistant.invocations.lock().unwrap();
    assert_eq!(calls[0]["arguments"]["content"], "First");
    assert_eq!(calls[1]["arguments"]["content"], "Second");
}
