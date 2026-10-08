use std::sync::{Arc, Mutex};

use lumvise_mcp_core::{LumviseMcpServer, McpApplication, McpApplicationError, McpTool};
use serde_json::{Value, json};
use std::io::{BufRead, Cursor, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, mpsc};
use std::time::Duration;

#[derive(Default)]
struct RecordingFakeApplication {
    calls: Mutex<Vec<(String, Value)>>,
}

impl McpApplication for RecordingFakeApplication {
    fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError> {
        Ok(vec![McpTool::new(
            "notes.create",
            "Create one note.",
            json!({
                "type": "object",
                "required": ["title"],
                "properties": { "title": { "type": "string" } }
            }),
        )])
    }

    fn invoke_tool(&self, name: &str, arguments: Value) -> Result<Value, McpApplicationError> {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_string(), arguments.clone()));
        Ok(json!({ "created": arguments["title"] }))
    }
}

#[test]
fn json_rpc_transport_lists_and_invokes_tools_through_generic_application() {
    let application = Arc::new(RecordingFakeApplication::default());
    let server = LumviseMcpServer::new(application.clone());

    let listed = request(
        &server,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    );
    let invoked = request(
        &server,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "notes.create",
                "arguments": { "title": "Architecture" }
            }
        }),
    );

    assert_eq!(listed["result"]["tools"][0]["name"], json!("notes.create"));
    assert_eq!(tool_payload(&invoked), json!({ "created": "Architecture" }));
    assert_eq!(
        *application.calls.lock().unwrap(),
        vec![(
            "notes.create".to_string(),
            json!({ "title": "Architecture" })
        )]
    );
}

fn request(server: &LumviseMcpServer, request: Value) -> Value {
    let response = server
        .handle_json_line(&request.to_string())
        .unwrap()
        .unwrap();
    serde_json::from_str(&response).unwrap()
}

fn tool_payload(response: &Value) -> Value {
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

struct FakeClientInput {
    lines: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    buffer: Cursor<Vec<u8>>,
}

impl BufRead for FakeClientInput {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.buffer.position() as usize == self.buffer.get_ref().len() {
            self.buffer = Cursor::new(self.lines.recv().unwrap_or_else(|_| Ok(Vec::new()))?);
        }
        self.buffer.fill_buf()
    }
    fn consume(&mut self, count: usize) {
        self.buffer.consume(count);
    }
}

impl Read for FakeClientInput {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let buffer = self.fill_buf()?;
        let count = bytes.len().min(buffer.len());
        bytes[..count].copy_from_slice(&buffer[..count]);
        self.consume(count);
        Ok(count)
    }
}

struct FakeClientOutput {
    responses: mpsc::Sender<String>,
    buffer: Vec<u8>,
    fail: Arc<AtomicBool>,
}

impl Write for FakeClientOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.fail.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fake client closed stdout",
            ));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let response = String::from_utf8(std::mem::take(&mut self.buffer)).unwrap();
        self.responses.send(response).map_err(std::io::Error::other)
    }
}

struct FakeMcpClient {
    lines: mpsc::Sender<std::io::Result<Vec<u8>>>,
    responses: mpsc::Receiver<String>,
    fail: Arc<AtomicBool>,
}

impl FakeMcpClient {
    fn streams() -> (Self, FakeClientInput, FakeClientOutput) {
        let (send, lines) = mpsc::channel();
        let (responses, receive) = mpsc::channel();
        let fail = Arc::new(AtomicBool::new(false));
        (
            Self {
                lines: send,
                responses: receive,
                fail: fail.clone(),
            },
            FakeClientInput {
                lines,
                buffer: Cursor::new(Vec::new()),
            },
            FakeClientOutput {
                responses,
                buffer: Vec::new(),
                fail,
            },
        )
    }
    fn send(&self, request: Value) {
        self.lines
            .send(Ok(format!("{request}\n").into_bytes()))
            .unwrap();
    }
    fn response(&self) -> Value {
        serde_json::from_str(&self.responses.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap()
    }
    fn initialize(&self) {
        self.send(json!({"id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"fake-client","version":"1"}}}));
        assert!(self.response().get("result").is_some());
        self.send(json!({"method":"notifications/initialized"}));
    }
}

struct FakeLifecycleApplication {
    events: mpsc::Sender<&'static str>,
    closed: (Mutex<bool>, Condvar),
}

impl FakeLifecycleApplication {
    fn new() -> (Arc<Self>, mpsc::Receiver<&'static str>) {
        let (events, receive) = mpsc::channel();
        (
            Arc::new(Self {
                events,
                closed: (Mutex::new(false), Condvar::new()),
            }),
            receive,
        )
    }
}

impl McpApplication for FakeLifecycleApplication {
    fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError> {
        Ok(Vec::new())
    }
    fn invoke_tool(&self, _: &str, _: Value) -> Result<Value, McpApplicationError> {
        self.events.send("invocation").unwrap();
        let closed = self.closed.0.lock().unwrap();
        let (closed, _) = self
            .closed
            .1
            .wait_timeout_while(closed, Duration::from_secs(5), |closed| !*closed)
            .unwrap();
        assert!(*closed, "disconnect must precede invocation draining");
        Ok(json!({}))
    }
    fn mcp_client_initialized(&self) -> Result<(), McpApplicationError> {
        self.events.send("initialized").unwrap();
        Ok(())
    }
    fn mcp_client_disconnected(&self) {
        *self.closed.0.lock().unwrap() = true;
        self.closed.1.notify_all();
        self.events.send("closed").unwrap();
    }
}

fn expect_event(events: &mpsc::Receiver<&'static str>, event: &str) {
    assert_eq!(events.recv_timeout(Duration::from_secs(5)).unwrap(), event);
}

#[test]
fn initialized_requires_valid_initialize_and_closed_transport_is_sticky() {
    let (application, events) = FakeLifecycleApplication::new();
    let server = LumviseMcpServer::new(application);
    server
        .handle_json_line(r#"{"method":"notifications/initialized"}"#)
        .unwrap();
    assert!(
        request(&server, json!({"id":1,"method":"initialize","params":{}}))
            .get("error")
            .is_some()
    );
    server
        .handle_json_line(r#"{"method":"notifications/initialized"}"#)
        .unwrap();
    assert!(events.try_recv().is_err());
    lumvise_mcp_core::run_stdio(server, Cursor::new(Vec::<u8>::new()), Vec::new()).unwrap();
    expect_event(&events, "closed");
    assert!(events.try_recv().is_err());
}

#[test]
fn eof_notifies_disconnect_before_draining_in_flight_invocation() {
    let (application, events) = FakeLifecycleApplication::new();
    let (client, input, output) = FakeMcpClient::streams();
    let worker = std::thread::spawn(move || {
        lumvise_mcp_core::run_stdio(LumviseMcpServer::new(application), input, output)
    });
    client.initialize();
    expect_event(&events, "initialized");
    client.send(json!({"id":2,"method":"tools/call","params":{"name":"blocking","arguments":{}}}));
    expect_event(&events, "invocation");
    drop(client);
    expect_event(&events, "closed");
    // The client also closed stdout; BrokenPipe after cleanup is expected.
    let _ = worker.join().unwrap();
    assert!(events.try_recv().is_err());
}

#[test]
fn reader_failure_notifies_disconnect_before_draining_in_flight_invocation() {
    let (application, events) = FakeLifecycleApplication::new();
    let (client, input, output) = FakeMcpClient::streams();
    let worker = std::thread::spawn(move || {
        lumvise_mcp_core::run_stdio(LumviseMcpServer::new(application), input, output)
    });
    client.initialize();
    expect_event(&events, "initialized");
    client.send(json!({"id":2,"method":"tools/call","params":{"name":"blocking","arguments":{}}}));
    expect_event(&events, "invocation");
    client
        .lines
        .send(Err(std::io::Error::other("fake client read failure")))
        .unwrap();
    expect_event(&events, "closed");
    assert!(worker.join().unwrap().is_err());
    assert!(events.try_recv().is_err());
}

#[test]
fn worker_write_failure_closes_transport_while_input_is_still_open() {
    let (application, events) = FakeLifecycleApplication::new();
    let (client, input, output) = FakeMcpClient::streams();
    let worker = std::thread::spawn(move || {
        lumvise_mcp_core::run_stdio(LumviseMcpServer::new(application), input, output)
    });
    client.initialize();
    expect_event(&events, "initialized");
    client.fail.store(true, Ordering::Release);
    client.send(json!({"id":2,"method":"tools/list"}));
    expect_event(&events, "closed");
    client.send(json!({"method":"notifications/initialized"}));
    drop(client);
    assert!(worker.join().unwrap().is_err());
    assert!(events.try_recv().is_err());
}

struct FakeDelayedBindingApplication {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    initialized: AtomicBool,
}

impl McpApplication for FakeDelayedBindingApplication {
    fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError> {
        Ok(Vec::new())
    }
    fn mcp_client_initialized(&self) -> Result<(), McpApplicationError> {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| McpApplicationError::invocation(error.to_string()))?;
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }
    fn invoke_tool(&self, _: &str, _: Value) -> Result<Value, McpApplicationError> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(McpApplicationError::invocation(
                "binding dispatched before initialization callback completed",
            ));
        }
        Ok(json!({"binding_status":"ready"}))
    }
}

#[test]
fn stdio_initialized_notification_precedes_immediately_following_binding_invocation() {
    let (entered, receive_entered) = mpsc::channel();
    let (release, receive_release) = mpsc::channel();
    let application = Arc::new(FakeDelayedBindingApplication {
        entered,
        release: Mutex::new(receive_release),
        initialized: AtomicBool::new(false),
    });
    let (client, input, output) = FakeMcpClient::streams();
    let worker = std::thread::spawn(move || {
        lumvise_mcp_core::run_stdio(LumviseMcpServer::new(application), input, output)
    });
    client.initialize();
    receive_entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    client.send(json!({"id":2,"method":"tools/call","params":{"name":"set_current_project","arguments":{"project_root":"/projects/a"}}}));
    let premature = client.responses.recv_timeout(Duration::from_millis(100));
    release.send(()).unwrap();
    if premature.is_err() {
        assert_eq!(tool_payload(&client.response())["binding_status"], "ready");
    }
    drop(client);
    worker.join().unwrap().unwrap();
    assert!(
        premature.is_err(),
        "MCP binding must wait for initialized notification: {premature:?}"
    );
}
