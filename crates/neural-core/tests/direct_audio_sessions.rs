//! Public provider boundary: real WebSockets, one persistent session, duplex PCM.
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{
    AudioSessionCommand as Command, AudioSessionEvent as Event, AudioSessionRequest, LlmRequest,
};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

struct NoCompletionHttp;
impl LlmHttpClient for NoCompletionHttp {
    fn post_json(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("direct audio must use WebSocket")
    }
    fn stream_text(&self, _: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!("direct audio must not use text streaming")
    }
}

struct FakeLiveAudioServer {
    endpoint: String,
    task: JoinHandle<Vec<Value>>,
}
impl FakeLiveAudioServer {
    fn start(gemini: bool, resume: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}/live", listener.local_addr().unwrap());
        let task = thread::spawn(move || run_server(listener, gemini, resume));
        Self { endpoint, task }
    }
}

fn accept_audio(listener: &TcpListener) -> WebSocket<TcpStream> {
    let (stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    tungstenite::accept(stream).unwrap()
}
fn read_json(socket: &mut WebSocket<TcpStream>) -> Value {
    serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap()
}
fn send_json(socket: &mut WebSocket<TcpStream>, event: Value) {
    socket
        .send(Message::Text(event.to_string().into()))
        .unwrap();
}
fn ready(socket: &mut WebSocket<TcpStream>, gemini: bool) {
    send_json(
        socket,
        if gemini {
            json!({"setupComplete":{}})
        } else {
            json!({"type":"session.updated","session":{"id":"persistent"}})
        },
    );
}
fn run_server(listener: TcpListener, gemini: bool, resume: bool) -> Vec<Value> {
    let mut socket = accept_audio(&listener);
    let mut received = vec![read_json(&mut socket)];
    ready(&mut socket, gemini);
    for turn in 1..=2 {
        received.push(read_json(&mut socket));
        if !gemini {
            received.push(read_json(&mut socket));
        }
        send_audio(&mut socket, gemini, turn);
        received.push(read_json(&mut socket)); // Microphone reaches server before generation ends.
        send_json(
            &mut socket,
            if gemini {
                json!({"serverContent":{"turnComplete":true}})
            } else {
                json!({"type":"response.done","response":{"id":format!("r{turn}"),"status":"completed"}})
            },
        );
        if resume && turn == 1 {
            send_json(
                &mut socket,
                json!({"sessionResumptionUpdate":{"resumable":true,"newHandle":"resume-first-turn"}}),
            );
            send_json(&mut socket, json!({"goAway":{"timeLeft":"30s"}}));
            assert!(matches!(socket.read().unwrap(), Message::Close(_)));
            socket = accept_audio(&listener);
            received.push(read_json(&mut socket));
            ready(&mut socket, gemini);
        }
    }
    assert!(matches!(socket.read().unwrap(), Message::Close(_)));
    received
}
fn send_audio(socket: &mut WebSocket<TcpStream>, gemini: bool, turn: u32) {
    if gemini {
        send_json(
            socket,
            json!({"serverContent":{"inputTranscription":{"text":"Question"},"outputTranscription":{"text":"Answer"},"modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=24000","data":"AQACAA=="}}]}}}),
        );
        return;
    }
    send_json(
        socket,
        json!({"type":"response.created","response":{"id":format!("r{turn}")}}),
    );
    send_json(
        socket,
        json!({"type":"response.output_audio.delta","response_id":format!("r{turn}"),"item_id":format!("i{turn}"),"delta":"AQACAA=="}),
    );
}
fn exercise_provider(gemini: bool, resume: bool) -> Vec<Value> {
    let server = FakeLiveAudioServer::start(gemini, resume);
    let provider_id = if gemini { "gemini" } else { "openai_realtime" };
    let config = LlmProviderConfig {
        provider_id: provider_id.into(),
        kind: if gemini {
            LlmProviderKind::Gemini
        } else {
            LlmProviderKind::OpenAiRealtime
        },
        model: if gemini {
            "gemini-test-live"
        } else {
            "gpt-realtime"
        }
        .into(),
        endpoint: Some(server.endpoint.clone()),
        credential: Some("fake-test-key".into()),
        completion_concurrency: None,
        spawn: gemini.then(|| SpawnConfig {
            command: "must-not-spawn".into(),
            args: vec![],
            timeout_ms: 1000,
        }),
    };
    let registry =
        LlmProviderRegistry::from_configs(vec![config], Arc::new(NoCompletionHttp)).unwrap();
    let provider = registry.provider_handle(provider_id).unwrap();
    let (send, receive) = mpsc::channel();
    send.send(Command::Text("first".into())).unwrap();
    let mut finished = 0;
    let mut ready_count = 0;
    let mut audio_count = 0;
    let request: LlmRequest = serde_json::from_value(
        json!({"messages":[{"role":"user","content":"first"}],"stream":true}),
    )
    .unwrap();
    provider
        .run_audio_session(
            AudioSessionRequest {
                conversation: request,
                voice: None,
            },
            receive,
            &mut |event| {
                match event {
                    Event::Ready { .. } => {
                        ready_count += 1;
                        if resume && ready_count == 2 {
                            send.send(Command::Text("second".into())).unwrap();
                        }
                    }
                    Event::Audio {
                        pcm, sample_rate, ..
                    } => {
                        assert_eq!(pcm, [1, 0, 2, 0]);
                        assert_eq!(sample_rate, 24000);
                        audio_count += 1;
                        send.send(Command::Pcm(vec![3, 0, 4, 0])).unwrap();
                    }
                    Event::ResponseFinished { .. } => {
                        finished += 1;
                        if finished == 2 {
                            send.send(Command::Close).unwrap();
                        } else if !resume {
                            send.send(Command::Text("second".into())).unwrap();
                        }
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(audio_count, 2);
    assert_eq!(ready_count, if resume { 2 } else { 1 });
    server.task.join().unwrap()
}

#[test]
fn gemini_public_provider_streams_microphone_during_output_and_reuses_connection() {
    let sent = exercise_provider(true, false);
    assert_eq!(
        sent[0]["setup"]["generationConfig"]["responseModalities"],
        json!(["AUDIO"])
    );
    assert_eq!(
        sent.iter()
            .filter(|event| event.pointer("/realtimeInput/audio").is_some())
            .count(),
        2
    );
    assert_eq!(
        sent.iter()
            .filter(|event| event.get("setup").is_some())
            .count(),
        1
    );
}
#[test]
fn openai_public_provider_streams_microphone_during_output_and_reuses_connection() {
    let sent = exercise_provider(false, false);
    assert_eq!(sent[0]["session"]["output_modalities"], json!(["audio"]));
    assert_eq!(
        sent.iter()
            .filter(|event| event["type"] == "input_audio_buffer.append")
            .count(),
        2
    );
    assert_eq!(
        sent.iter()
            .filter(|event| event["type"] == "session.update")
            .count(),
        1
    );
}
#[test]
fn gemini_server_rotation_resumes_the_same_conversation_without_replaying_opening() {
    let sent = exercise_provider(true, true);
    let setups: Vec<_> = sent
        .iter()
        .filter(|event| event.get("setup").is_some())
        .collect();
    assert_eq!(setups.len(), 2);
    assert_eq!(
        setups[1]["setup"]["sessionResumption"]["handle"],
        "resume-first-turn"
    );
    assert_eq!(
        sent.iter()
            .filter(|event| event.pointer("/clientContent/turns/0/parts/0/text")
                == Some(&json!("first")))
            .count(),
        1
    );
}

struct FakeAudioMcpServer {
    url: String,
    task: JoinHandle<Value>,
}
impl FakeAudioMcpServer {
    fn start(microphone: mpsc::Sender<Command>, delivered: mpsc::Receiver<()>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let (mut catalog, _) = listener.accept().unwrap();
            let request = read_http_json(&mut catalog);
            assert_eq!(request["method"], "tools/list");
            write_http_json(
                &mut catalog,
                json!({"jsonrpc":"2.0","id":1,"result":{"tools":[
                    {"name":"canvas_get","description":"Read current drawing","inputSchema":{"type":"object"}},
                    {"name":"builtin_assistant__assistant_respond","description":"Legacy speech","inputSchema":{"type":"object"}}
                ]}}),
            );
            let (mut invocation, _) = listener.accept().unwrap();
            let request = read_http_json(&mut invocation);
            assert_eq!(request["params"]["name"], "canvas_get");
            microphone.send(Command::Pcm(vec![3, 0, 4, 0])).unwrap();
            delivered
                .recv_timeout(Duration::from_secs(3))
                .expect("microphone must flow while MCP is waiting");
            write_http_json(
                &mut invocation,
                json!({"jsonrpc":"2.0","id":request["id"],"result":{"canvas":"current"}}),
            );
            request
        });
        Self { url, task }
    }
}
fn read_http_json(stream: &mut TcpStream) -> Value {
    use std::io::Read;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
    }
    let header = String::from_utf8(request).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}
fn write_http_json(stream: &mut TcpStream, value: Value) {
    use std::io::Write;
    let body = value.to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
}

#[test]
fn gemini_scoped_mcp_call_does_not_block_microphone_or_duplicate_local_speech() {
    let (send, receive) = mpsc::channel();
    let (delivered, delivery) = mpsc::channel();
    let mcp = FakeAudioMcpServer::start(send.clone(), delivery);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/live", listener.local_addr().unwrap());
    let vendor = thread::spawn(move || {
        let mut socket = accept_audio(&listener);
        let setup = read_json(&mut socket);
        let tools = setup["setup"]["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "canvas_get");
        ready(&mut socket, true);
        read_json(&mut socket);
        send_json(
            &mut socket,
            json!({"toolCall":{"functionCalls":[{"id":"canvas-1","name":"canvas_get","args":{}}]}}),
        );
        assert_eq!(
            read_json(&mut socket)["realtimeInput"]["audio"]["data"],
            "AwAEAA=="
        );
        delivered.send(()).unwrap();
        assert_eq!(
            read_json(&mut socket)["toolResponse"]["functionResponses"][0]["response"]["result"]["canvas"],
            "current"
        );
        send_audio(&mut socket, true, 1);
        send_json(&mut socket, json!({"serverContent":{"turnComplete":true}}));
        assert!(matches!(socket.read().unwrap(), Message::Close(_)));
    });
    let config = LlmProviderConfig {
        provider_id: "gemini".into(),
        kind: LlmProviderKind::Gemini,
        model: "gemini-test-live".into(),
        endpoint: Some(endpoint),
        credential: Some("fake-test-key".into()),
        completion_concurrency: None,
        spawn: None,
    };
    let registry =
        LlmProviderRegistry::from_configs(vec![config], Arc::new(NoCompletionHttp)).unwrap();
    let request = serde_json::from_value(
        json!({"messages":[{"role":"user","content":"Explain the drawing"}],"stream":true,
        "mcp_servers":[{"name":"scoped-canvas","url":mcp.url}]}),
    )
    .unwrap();
    send.send(Command::Text("Explain the drawing".into()))
        .unwrap();
    registry
        .provider_handle("gemini")
        .unwrap()
        .run_audio_session(
            AudioSessionRequest {
                conversation: request,
                voice: None,
            },
            receive,
            &mut |event| {
                if matches!(event, Event::ResponseFinished { .. }) {
                    send.send(Command::Close).unwrap();
                }
                Ok(())
            },
        )
        .unwrap();
    vendor.join().unwrap();
    mcp.task.join().unwrap();
}
