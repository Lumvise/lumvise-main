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
    speech_probe: Option<Arc<Mutex<Vec<Value>>>>,
}

impl StreamedModel {
    fn new(rounds: Vec<Vec<String>>) -> Self {
        Self {
            rounds: Arc::new(Mutex::new(rounds.into())),
            ..Self::default()
        }
    }

    fn registry(&self) -> LlmProviderRegistry {
        self.registry_for("openrouter", LlmProviderKind::OpenRouter)
    }

    fn registry_for(&self, provider_id: &str, kind: LlmProviderKind) -> LlmProviderRegistry {
        LlmProviderRegistry::from_configs(
            vec![LlmProviderConfig {
                provider_id: provider_id.into(),
                kind,
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
        for (index, chunk) in chunks.into_iter().enumerate() {
            if index == 1
                && let Some(calls) = &self.speech_probe
            {
                assert_eq!(
                    calls.lock().unwrap().len(),
                    1,
                    "first sentence must reach the scoped speech tool before the next model chunk"
                );
            }
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
        json!({"tools":[{"name":"assistant_respond","description":"Speak","inputSchema":{"type":"object","properties":{"content":{"type":"string"},"final":{"type":"boolean"}},"required":["content"]}}, {"name":"assistant_finish","description":"Close session","inputSchema":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}}, {"name":"lookup","description":"Read a record","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}}]})
    } else {
        calls.lock().unwrap().push(rpc["params"].clone());
        if outcome["echo_response"] == true {
            let content = calls
                .lock()
                .unwrap()
                .iter()
                .filter_map(|call| call["arguments"]["content"].as_str())
                .filter(|content| !content.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            json!({"structuredContent":{"decision":"assistant_responded","state":{
                "last_response":content,"app_response_finalized":rpc["params"]["arguments"]["final"] == true
            }}})
        } else {
            outcome.clone()
        }
    };
    let body = json!({"jsonrpc":"2.0","id":rpc["id"],"result":result}).to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
}

fn streamed_request(assistant: &ScopedAssistant) -> LlmRequest {
    let mut request = assistant.request("streamed-speech");
    request.options.streamed_response_tool = Some("assistant_respond".into());
    request
}

#[test]
fn streamed_narration_preserves_tool_order_and_closes_only_the_last_round() {
    let assistant = ScopedAssistant::start(json!({"echo_response":true}));
    let model = StreamedModel::new(vec![
        vec![
            event(json!({"content":"I will check that record. "}), Value::Null),
            event(
                json!({"tool_calls":[{"index":0,"id":"read","function":{"name":"lookup","arguments":"{\"id\":"}}]}),
                Value::Null,
            ),
            event(
                json!({"tool_calls":[{"index":0,"function":{"arguments":"\"record\"}"}}]}),
                json!("tool_calls"),
            ),
        ],
        vec![event(
            json!({"content":"Here is the result."}),
            json!("stop"),
        )],
    ]);
    let response = model
        .registry()
        .complete("openrouter", &streamed_request(&assistant))
        .unwrap();
    assert_eq!(
        response.content,
        "I will check that record. Here is the result."
    );
    let calls = assistant.invocations.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|call| call["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["assistant_respond", "lookup", "assistant_respond"]
    );
    assert_eq!(calls[0]["arguments"]["final"], false);
    assert_eq!(calls[1]["arguments"], json!({"id":"record"}));
    assert_eq!(calls[2]["arguments"]["final"], true);
}

#[test]
fn cancelled_or_rejected_streaming_delivery_stops_generation_without_fallback() {
    for outcome in [
        json!({"structuredContent":{"decision":"cancelled"}}),
        json!({"isError":true,"content":[{"type":"text","text":"not available"}]}),
    ] {
        let assistant = ScopedAssistant::start(outcome);
        let model = StreamedModel::new(vec![vec![
            event(json!({"content":"First sentence. "}), Value::Null),
            event(json!({"content":"Must never speak this."}), json!("stop")),
        ]]);
        assert!(
            model
                .registry()
                .complete("openrouter", &streamed_request(&assistant))
                .is_err()
        );
        assert_eq!(assistant.invocations.lock().unwrap().len(), 1);
        assert_eq!(model.requests.lock().unwrap().len(), 1);
        assert!(!model.read_tail.load(Ordering::SeqCst));
    }
}

#[test]
fn streamed_reply_preserves_unicode_and_split_frames_and_sends_empty_final_once() {
    let assistant = ScopedAssistant::start(json!({"echo_response":true}));
    let first = event(json!({"content":"こんにちは 🐼. "}), Value::Null);
    let model = StreamedModel::new(vec![vec![
        first[..10].into(),
        first[10..].into(),
        event(
            json!({"reasoning":"Do not speak this.", "content":"The value is 3."}),
            Value::Null,
        ),
        event(json!({"content":"14. "}), Value::Null),
        event(json!({}), json!("stop")),
    ]]);
    let response = model
        .registry()
        .complete("openrouter", &streamed_request(&assistant))
        .unwrap();
    assert_eq!(response.content, "こんにちは 🐼. The value is 3.14.");
    let calls = assistant.invocations.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1]["arguments"]["content"], "The value is 3.14.");
    assert_eq!(calls[2]["arguments"], json!({"content":"", "final":true}));
}

#[test]
fn malformed_or_truncated_stream_never_claims_final_delivery() {
    for tail in [
        "data: {broken}\n\n".into(),
        event(json!({}), json!("length")),
    ] {
        let assistant = ScopedAssistant::start(json!({"echo_response":true}));
        let model = StreamedModel::new(vec![vec![
            event(json!({"content":"A partial answer. "}), Value::Null),
            tail,
        ]]);
        assert!(
            model
                .registry()
                .complete("openrouter", &streamed_request(&assistant))
                .is_err()
        );
        let calls = assistant.invocations.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["arguments"]["final"], false);
    }
}

#[test]
fn serialized_tool_leaks_never_reach_streamed_speech() {
    for text in [
        "<tool_call>lookup<arg_key>id</arg_key><arg_value>record.</arg_value></tool_call>",
        "call:lookup{id:record.}",
    ] {
        let assistant = ScopedAssistant::start(json!({"echo_response":true}));
        let model = StreamedModel::new(vec![vec![event(json!({"content":text}), json!("stop"))]]);
        assert!(
            model
                .registry()
                .complete("openrouter", &streamed_request(&assistant))
                .is_err()
        );
        assert!(assistant.invocations.lock().unwrap().is_empty());
    }
}

#[test]
fn hidden_response_tool_is_not_executed_a_second_time() {
    let assistant = ScopedAssistant::start(json!({"echo_response":true}));
    let model = StreamedModel::new(vec![
        speech_round(),
        vec![event(json!({"content":"Only speak once."}), json!("stop"))],
    ]);
    let result = model
        .registry()
        .complete("openrouter", &streamed_request(&assistant))
        .unwrap();
    assert_eq!(result.content, "Only speak once.");
    assert_eq!(assistant.invocations.lock().unwrap().len(), 1);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
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
fn opted_in_speech_reaches_assistant_before_the_model_finishes() {
    let assistant = ScopedAssistant::start(json!({"echo_response":true}));
    let mut model = StreamedModel::new(vec![vec![
        event(json!({"content":"Hello there. "}), Value::Null),
        event(json!({"content":"This arrives later."}), json!("stop")),
    ]]);
    model.speech_probe = Some(assistant.invocations.clone());
    let mut request = serde_json::to_value(assistant.request("streamed-speech")).unwrap();
    request["options"] = json!({"streamed_response_tool":"assistant_respond"});
    let request: LlmRequest = serde_json::from_value(request).unwrap();
    let result = model
        .registry_for("lumvise_cloud", LlmProviderKind::OpenAiCompatible)
        .complete("lumvise_cloud", &request)
        .unwrap();
    assert_eq!(result.content, "Hello there. This arrives later.");
    let calls = assistant.invocations.lock().unwrap();
    assert_eq!(
        calls[0]["arguments"],
        json!({"content":"Hello there.","final":false})
    );
    assert_eq!(calls.last().unwrap()["arguments"]["final"], true);
    let requests = model.requests.lock().unwrap();
    assert!(
        requests[0].payload["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["function"]["name"] != "assistant_respond")
    );
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
fn hosted_openai_tool_calls_honor_requested_streaming() {
    let assistant = ScopedAssistant::start(acknowledged(true));
    let model = StreamedModel::new(vec![speech_round()]);
    let registry = LlmProviderRegistry::from_configs(
        vec![LlmProviderConfig {
            provider_id: "lumvise_cloud".into(),
            kind: LlmProviderKind::OpenAiCompatible,
            model: "provider-default".into(),
            endpoint: Some("https://unused.test/ai".into()),
            credential: None,
            completion_concurrency: None,
            spawn: None,
        }],
        Arc::new(model.clone()),
    )
    .unwrap();
    let response = registry
        .complete("lumvise_cloud", &assistant.request("workspace:voice"))
        .unwrap();
    assert_eq!(response.content, "Hello 🐼");
    assert_eq!(model.requests.lock().unwrap()[0].payload["stream"], true);
    assert_eq!(assistant.invocations.lock().unwrap().len(), 1);
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

#[test]
fn canvas_images_reach_the_next_model_round_after_all_parallel_tool_receipts() {
    let assistant = ScopedAssistant::start(json!({"content":[
        {"type":"text","text":"Canvas revision 4"},
        {"type":"image","mimeType":"image/png","data":"rendered-pixels"}
    ]}));
    let calls = event(
        json!({"tool_calls":[
            {"index":0,"id":"preview-a","function":{"name":"lookup","arguments":"{\"id\":\"a\"}"}},
            {"index":1,"id":"preview-b","function":{"name":"lookup","arguments":"{\"id\":\"b\"}"}}
        ]}),
        json!("tool_calls"),
    );
    let model = StreamedModel::new(vec![
        vec![calls],
        vec![event(
            json!({"content":"The layout is readable."}),
            json!("stop"),
        )],
    ]);
    model
        .registry()
        .complete("openrouter", &assistant.request("canvas-review"))
        .unwrap();
    let requests = model.requests.lock().unwrap();
    let messages = requests[1].payload["messages"].as_array().unwrap();
    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|message| message["role"].as_str())
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "user", "user"]);
    assert!(!messages[2].to_string().contains("rendered-pixels"));
    assert_eq!(
        messages[4]["content"][1]["image_url"]["url"],
        "data:image/png;base64,rendered-pixels"
    );
}
