use super::*;
use crate::llm_providers::tool_invocation::McpToolCatalog;
use parking_lot::Mutex;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use tungstenite::WebSocket;

const LIVE_SERVER_DELTA: &str =
    r#"{"serverContent":{"modelTurn":{"parts":[{"text":"live ok"}]},"turnComplete":true}}"#;
const GEMINI_TOOL_CALL: &str = r#"{"toolCall":{"functionCalls":[{"id":"call-1","name":"builtin_assistant__assistant_respond","args":{"content":"hi from live tool"}}]}}"#;

#[test]
fn parses_text_delta_from_server_content() {
    let value = json!({
        "serverContent": {
            "modelTurn": { "parts": [{ "text": "hello" }] }
        }
    });

    assert_eq!(
        live_events_from_value(&value),
        vec![LlmStreamEvent::ContentDelta {
            text: "hello".to_string()
        }]
    );
}

#[test]
fn parses_session_resumption_update_from_live_server() {
    let value = json!({
        "sessionResumptionUpdate": {
            "newHandle": "gemini-live-session-1"
        }
    });

    assert_eq!(
        live_events_from_value(&value),
        vec![LlmStreamEvent::Session {
            provider_session_id: "gemini-live-session-1".to_string(),
        }]
    );
}

#[test]
fn setup_message_includes_existing_session_resumption_handle() {
    let mut turn = test_turn();
    turn.session_resumption_handle = Some("gemini-live-session-1".to_string());

    let message = setup_message(
        &turn,
        &GeminiLiveMcpTools {
            tools: McpToolCatalog::empty("gemini"),
            function_declarations: Vec::new(),
        },
    );

    assert_eq!(
        message["setup"]["sessionResumption"]["handle"],
        "gemini-live-session-1"
    );
}

#[test]
fn builds_realtime_audio_message_with_base64_pcm() {
    let input = GeminiLiveInput {
        kind: LlmModalityInputKind::LiveAudioChunk,
        media_type: "audio/pcm;rate=16000".to_string(),
        bytes: vec![1, 2, 3],
    };

    let message = realtime_input_message(&input);

    assert_eq!(
        message["realtimeInput"]["audio"]["mimeType"],
        "audio/pcm;rate=16000"
    );
    assert_eq!(message["realtimeInput"]["audio"]["data"], "AQID");
}

#[test]
fn builds_realtime_video_message_for_screen_frame() {
    let input = GeminiLiveInput {
        kind: LlmModalityInputKind::ScreenFrame,
        media_type: "image/jpeg".to_string(),
        bytes: vec![4, 5],
    };

    let message = realtime_input_message(&input);

    assert_eq!(message["realtimeInput"]["video"]["mimeType"], "image/jpeg");
    assert_eq!(message["realtimeInput"]["video"]["data"], "BAU=");
}

#[test]
fn input_trace_event_records_audio_and_video_bytes_without_payload() {
    let audio = GeminiLiveInput {
        kind: LlmModalityInputKind::LiveAudioChunk,
        media_type: "audio/pcm;rate=16000".to_string(),
        bytes: vec![1, 2, 3],
    };
    let video = GeminiLiveInput {
        kind: LlmModalityInputKind::ScreenFrame,
        media_type: "image/png".to_string(),
        bytes: vec![4, 5],
    };

    assert_eq!(
        input_trace_event(&audio),
        GeminiLiveTraceEvent {
            stage: GeminiLiveTraceStage::RealtimeAudioSent,
            media_type: Some("audio/pcm;rate=16000".to_string()),
            bytes: Some(3),
            text_chars: None,
        }
    );
    assert_eq!(
        input_trace_event(&video),
        GeminiLiveTraceEvent {
            stage: GeminiLiveTraceStage::RealtimeVideoSent,
            media_type: Some("image/png".to_string()),
            bytes: Some(2),
            text_chars: None,
        }
    );
}

#[test]
fn timed_out_websocket_error_reports_recoverable_provider_timeout() {
    let turn = test_turn();
    let error = live_transport_error(
        &turn,
        tungstenite::Error::Io(std::io::Error::new(ErrorKind::TimedOut, "slow server")),
    );

    assert!(live_error_is_timeout(&error));
    assert!(error.to_string().contains("timed out after 30s"));
    assert!(error.to_string().contains("gemini-live-test"));
}

#[test]
fn websocket_transport_streams_audio_video_and_text_over_live_socket() {
    let server = start_live_test_server();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let transport = GeminiLiveWebSocketTransport::new_with_trace(
        Some(server.endpoint),
        trace_sink_for(trace.clone()),
    );
    let mut events = Vec::new();

    transport
        .stream_turn(
            live_socket_turn(),
            StreamControl::unbounded(),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "live ok".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
    assert_eq!(server.handle.join().unwrap().len(), 5);
    assert_live_trace_stages(&trace);
}

#[test]
fn websocket_transport_executes_mcp_tool_call_and_sends_tool_response() {
    let mcp = FakeGeminiMcpServer::spawn();
    let server = start_tool_call_live_test_server();
    let transport = GeminiLiveWebSocketTransport::new(Some(server.endpoint));
    let mut events = Vec::new();

    transport
        .stream_turn(
            live_socket_turn_with_mcp(mcp.url()),
            StreamControl::unbounded(),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
    let live_messages = server.handle.join().unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "live ok".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
    assert!(live_messages[0].contains("functionDeclarations"));
    assert!(live_messages[0].contains("builtin_assistant__assistant_respond"));
    assert!(live_messages[5].contains("toolResponse"));
    assert!(live_messages[5].contains("call-1"));
    assert_eq!(
        mcp.tool_calls(),
        vec!["builtin_assistant__assistant_respond"]
    );
}

#[test]
fn websocket_transport_reports_close_before_turn_complete() {
    let server = start_closing_live_test_server();
    let transport = GeminiLiveWebSocketTransport::new(Some(server.endpoint));
    let mut events = Vec::new();

    let error = transport
        .stream_turn(
            live_socket_turn(),
            StreamControl::unbounded(),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("closed before turn complete"));
    assert!(error.contains("gemini-live-test"));
    assert!(events.is_empty());
    assert_eq!(server.handle.join().unwrap().len(), 5);
}

#[test]
fn live_closed_error_includes_close_frame_reason() {
    let frame = CloseFrame {
        code: tungstenite::protocol::frame::coding::CloseCode::Policy,
        reason: "bad auth".into(),
    };
    let error = live_closed_error(&test_turn(), Some(&frame)).to_string();

    assert!(error.contains("close code"));
    assert!(error.contains("bad auth"));
}

#[test]
fn websocket_transport_cancellation_does_not_emit_complete() {
    let server = start_live_test_server();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let transport = GeminiLiveWebSocketTransport::new_with_trace(
        Some(server.endpoint),
        trace_sink_for(trace.clone()),
    );
    let mut events = Vec::new();

    transport
        .stream_turn(
            live_socket_turn(),
            StreamControl::cancel_after(1),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "live ok".to_string()
            },
            LlmStreamEvent::Cancelled,
        ]
    );
    assert_eq!(server.handle.join().unwrap().len(), 5);
    assert_cancelled_trace_stages(&trace);
}

fn test_turn() -> GeminiLiveTurn {
    GeminiLiveTurn {
        provider_id: "gemini".to_string(),
        model: "gemini-live-test".to_string(),
        credential: GeminiLiveCredential::ApiKey("key".to_string()),
        session_resumption_handle: None,
        user_text: "hello".to_string(),
        mcp_servers: Vec::new(),
        inputs: Vec::new(),
    }
}

#[test]
fn live_connect_request_uses_api_key_query_for_api_key_credentials() {
    let request = live_connect_request(
        "ws://127.0.0.1/live?alt=json",
        &GeminiLiveCredential::ApiKey("api-key".to_string()),
    )
    .unwrap();

    assert_eq!(request.uri(), "ws://127.0.0.1/live?alt=json&key=api-key");
    assert!(request.headers().get(AUTHORIZATION).is_none());
}

#[test]
fn live_connect_request_uses_bearer_header_for_oauth_credentials() {
    let request = live_connect_request(
        "ws://127.0.0.1/live",
        &GeminiLiveCredential::OAuthBearer("oauth-token".to_string()),
    )
    .unwrap();

    assert_eq!(request.uri(), "ws://127.0.0.1/live");
    assert_eq!(
        request.headers().get(AUTHORIZATION).unwrap(),
        "Bearer oauth-token"
    );
}

#[test]
fn live_connect_request_uses_access_token_query_for_ephemeral_tokens() {
    let request = live_connect_request(
        DEFAULT_GEMINI_LIVE_ENDPOINT,
        &GeminiLiveCredential::EphemeralToken("short-token".to_string()),
    )
    .unwrap();

    assert_eq!(
        request.uri().to_string(),
        format!("{DEFAULT_GEMINI_LIVE_CONSTRAINED_ENDPOINT}?access_token=short-token")
    );
    assert!(request.headers().get(AUTHORIZATION).is_none());
}

struct LiveTestServer {
    endpoint: String,
    handle: JoinHandle<Vec<String>>,
}

struct FakeGeminiMcpServer {
    url: String,
    calls: Arc<Mutex<Vec<String>>>,
    _thread: JoinHandle<()>,
}

impl FakeGeminiMcpServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let thread_calls = Arc::clone(&calls);
        let handle = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                handle_gemini_mcp_connection(stream, &thread_calls);
            }
        });
        Self {
            url,
            calls,
            _thread: handle,
        }
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn tool_calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
}

fn start_live_test_server() -> LiveTestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/ws", listener.local_addr().unwrap());
    let handle = thread::spawn(move || run_live_test_server(listener));
    LiveTestServer { endpoint, handle }
}

fn start_closing_live_test_server() -> LiveTestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/ws", listener.local_addr().unwrap());
    let handle = thread::spawn(move || run_closing_live_test_server(listener));
    LiveTestServer { endpoint, handle }
}

fn start_tool_call_live_test_server() -> LiveTestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/ws", listener.local_addr().unwrap());
    let handle = thread::spawn(move || run_tool_call_live_test_server(listener));
    LiveTestServer { endpoint, handle }
}

fn run_live_test_server(listener: TcpListener) -> Vec<String> {
    let (stream, _) = listener.accept().unwrap();
    let mut socket = tungstenite::accept(stream).unwrap();
    let mut messages = Vec::new();
    receive_setup(&mut socket, &mut messages);
    send_server_json(&mut socket, r#"{"setupComplete":{}}"#);
    receive_live_inputs(&mut socket, &mut messages);
    send_server_json(&mut socket, LIVE_SERVER_DELTA);
    messages
}

fn run_closing_live_test_server(listener: TcpListener) -> Vec<String> {
    let (stream, _) = listener.accept().unwrap();
    let mut socket = tungstenite::accept(stream).unwrap();
    let mut messages = Vec::new();
    receive_setup(&mut socket, &mut messages);
    send_server_json(&mut socket, r#"{"setupComplete":{}}"#);
    receive_live_inputs(&mut socket, &mut messages);
    socket.close(None).unwrap();
    messages
}

fn run_tool_call_live_test_server(listener: TcpListener) -> Vec<String> {
    let (stream, _) = listener.accept().unwrap();
    let mut socket = tungstenite::accept(stream).unwrap();
    let mut messages = Vec::new();
    receive_setup(&mut socket, &mut messages);
    send_server_json(&mut socket, r#"{"setupComplete":{}}"#);
    receive_live_inputs(&mut socket, &mut messages);
    send_server_json(&mut socket, GEMINI_TOOL_CALL);
    messages.push(read_client_text(&mut socket));
    send_server_json(&mut socket, LIVE_SERVER_DELTA);
    messages
}

fn receive_setup(socket: &mut WebSocket<TcpStream>, messages: &mut Vec<String>) {
    let setup = read_client_text(socket);
    assert_text_contains(&setup, &["setup", "models/gemini-live-test"]);
    messages.push(setup);
}

fn receive_live_inputs(socket: &mut WebSocket<TcpStream>, messages: &mut Vec<String>) {
    for expected_parts in expected_live_input_parts() {
        let message = read_client_text(socket);
        assert_text_contains(&message, &expected_parts);
        messages.push(message);
    }
}

fn expected_live_input_parts() -> Vec<Vec<&'static str>> {
    vec![
        vec!["realtimeInput", "text", "hello live"],
        vec!["realtimeInput", "audio", "audio/pcm;rate=16000", "AQID"],
        vec!["realtimeInput", "video", "image/png", "BAU="],
        vec!["realtimeInput", "audioStreamEnd"],
    ]
}

fn read_client_text(socket: &mut WebSocket<TcpStream>) -> String {
    socket.read().unwrap().into_text().unwrap().to_string()
}

fn send_server_json(socket: &mut WebSocket<TcpStream>, text: &str) {
    socket.send(Message::Text(text.to_string().into())).unwrap();
}

fn handle_gemini_mcp_connection(mut stream: TcpStream, calls: &Arc<Mutex<Vec<String>>>) {
    let mut buffer = [0; 8192];
    let len = stream.read(&mut buffer).unwrap();
    let request = String::from_utf8_lossy(&buffer[..len]);
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("{}");
    let payload: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let response = if payload["method"] == json!("tools/list") {
        gemini_mcp_tools_response()
    } else {
        let name = payload["params"]["name"].as_str().unwrap_or_default();
        calls.lock().push(name.to_string());
        json!({
            "jsonrpc": "2.0",
            "id": payload["id"].clone(),
            "result": { "ok": true }
        })
    };
    write_http_json(&mut stream, response);
}

fn gemini_mcp_tools_response() -> Value {
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

fn assert_text_contains(text: &str, expected_parts: &[&str]) {
    for expected in expected_parts {
        assert!(
            text.contains(expected),
            "expected `{text}` to contain `{expected}`"
        );
    }
}

fn trace_sink_for(trace: Arc<Mutex<Vec<GeminiLiveTraceEvent>>>) -> Arc<GeminiLiveTraceSink> {
    Arc::new(move |event| trace.lock().push(event))
}

fn live_socket_turn() -> GeminiLiveTurn {
    GeminiLiveTurn {
        provider_id: "gemini".to_string(),
        model: "gemini-live-test".to_string(),
        credential: GeminiLiveCredential::ApiKey("key".to_string()),
        session_resumption_handle: None,
        user_text: "hello live".to_string(),
        mcp_servers: Vec::new(),
        inputs: vec![live_audio_input(), live_screen_frame_input()],
    }
}

fn live_socket_turn_with_mcp(mcp_url: String) -> GeminiLiveTurn {
    GeminiLiveTurn {
        mcp_servers: vec![LlmMcpServerConfig {
            name: "lumvise-assistant".to_string(),
            url: format!("{mcp_url}/mcp/sse/builtin.assistant/session-a"),
        }],
        ..live_socket_turn()
    }
}

fn live_audio_input() -> GeminiLiveInput {
    GeminiLiveInput {
        kind: LlmModalityInputKind::LiveAudioChunk,
        media_type: "audio/pcm;rate=16000".to_string(),
        bytes: vec![1, 2, 3],
    }
}

fn live_screen_frame_input() -> GeminiLiveInput {
    GeminiLiveInput {
        kind: LlmModalityInputKind::ScreenFrame,
        media_type: "image/png".to_string(),
        bytes: vec![4, 5],
    }
}

fn assert_live_trace_stages(trace: &Arc<Mutex<Vec<GeminiLiveTraceEvent>>>) {
    let stages = trace
        .lock()
        .iter()
        .map(|event| event.stage)
        .collect::<Vec<_>>();
    assert_eq!(stages, expected_live_trace_stages());
}

fn assert_cancelled_trace_stages(trace: &Arc<Mutex<Vec<GeminiLiveTraceEvent>>>) {
    let stages = trace
        .lock()
        .iter()
        .map(|event| event.stage)
        .collect::<Vec<_>>();
    assert!(stages.contains(&GeminiLiveTraceStage::Cancelled));
    assert!(!stages.contains(&GeminiLiveTraceStage::TurnCompleteReceived));
}

fn expected_live_trace_stages() -> Vec<GeminiLiveTraceStage> {
    vec![
        GeminiLiveTraceStage::WebsocketConnected,
        GeminiLiveTraceStage::SetupSent,
        GeminiLiveTraceStage::SetupCompleteReceived,
        GeminiLiveTraceStage::RealtimeTextSent,
        GeminiLiveTraceStage::RealtimeAudioSent,
        GeminiLiveTraceStage::RealtimeVideoSent,
        GeminiLiveTraceStage::AudioStreamEndSent,
        GeminiLiveTraceStage::TextDeltaReceived,
        GeminiLiveTraceStage::TurnCompleteReceived,
    ]
}
