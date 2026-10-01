use super::{McpToolCatalog, ToolOutcome};
use crate::llm_providers::LlmMcpServerConfig;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

struct FakeMcpServer {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    join: Option<JoinHandle<()>>,
}

impl FakeMcpServer {
    fn spawn(responses: Vec<(u16, Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp/sse/scoped", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);
        let join = thread::spawn(move || {
            for (status, response) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let (path, body) = read_request(&mut stream);
                captured_requests.lock().push((path, body));
                let body = response.to_string();
                let reason = if status == 200 { "OK" } else { "Bad Request" };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        Self {
            url,
            requests,
            join: Some(join),
        }
    }

    fn server_config(&self) -> LlmMcpServerConfig {
        LlmMcpServerConfig {
            name: "scoped".to_string(),
            url: self.url.clone(),
        }
    }
}

impl Drop for FakeMcpServer {
    fn drop(&mut self) {
        self.join.take().unwrap().join().unwrap();
    }
}

fn read_request(stream: &mut TcpStream) -> (String, Value) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let path = request_line.split_whitespace().nth(1).unwrap().to_string();
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    (path, serde_json::from_slice(&body).unwrap())
}

fn tool_list() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "tools": [{
            "name": "paint",
            "description": "Paint a color",
            "inputSchema": { "type": "object", "properties": { "color": { "type": "string" } } }
        }] }
    })
}

fn strict_tool_list() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "tools": [{
            "name": "paint",
            "description": "Paint a color",
            "inputSchema": {
                "type": "object",
                "required": ["color"],
                "properties": {
                    "color": { "type": "string", "enum": ["red", "blue"] }
                },
                "additionalProperties": false
            }
        }] }
    })
}

#[test]
fn invoker_rejects_invalid_arguments_before_dispatch_with_actionable_feedback() {
    let server = FakeMcpServer::spawn(vec![(200, strict_tool_list())]);
    let catalog = McpToolCatalog::discover("direct-api", &[server.server_config()]).unwrap();

    assert_eq!(
        catalog.tools()[0].input_schema,
        json!({
            "type": "object",
            "required": ["color"],
            "properties": {
                "color": { "type": "string", "enum": ["red", "blue"] }
            },
            "additionalProperties": false
        })
    );
    assert!(matches!(
        catalog.invoker().invoke("paint", json!({ "color": "purple" })),
        ToolOutcome::Validation(message)
            if message.contains("paint")
                && message.contains("color")
                && message.contains("red")
                && message.contains("blue")
                && message.contains("retry")
    ));
    assert_eq!(server.requests.lock().len(), 1);
}

#[test]
fn catalog_discovers_scoped_tools_and_invokes_the_trusted_message_route() {
    let server = FakeMcpServer::spawn(vec![
        (200, tool_list()),
        (
            200,
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "content": [{ "type": "text", "text": "painted" }] } }),
        ),
    ]);
    let catalog = McpToolCatalog::discover("direct-api", &[server.server_config()]).unwrap();

    assert_eq!(catalog.tools().len(), 1);
    assert_eq!(catalog.tools()[0].name, "paint");
    assert_eq!(catalog.tools()[0].description, "Paint a color");
    assert_eq!(
        catalog.tools()[0].input_schema,
        json!({ "type": "object", "properties": { "color": { "type": "string" } } })
    );
    let outcome = catalog
        .invoker()
        .invoke("paint", json!({ "color": "blue" }));
    assert!(
        matches!(outcome, ToolOutcome::Success(result) if result["content"][0]["text"] == "painted")
    );

    let requests = server.requests.lock();
    assert_eq!(requests[0].0, "/mcp/messages/scoped");
    assert_eq!(requests[0].1["method"], "tools/list");
    assert_eq!(
        requests[1].1["params"],
        json!({ "name": "paint", "arguments": { "color": "blue" } })
    );
}

#[test]
fn invoker_classifies_mcp_tool_error() {
    let server = FakeMcpServer::spawn(vec![
        (200, tool_list()),
        (
            200,
            json!({ "jsonrpc": "2.0", "id": 2, "result": { "isError": true, "content": [{ "type": "text", "text": "not allowed" }] } }),
        ),
    ]);
    let catalog = McpToolCatalog::discover("live", &[server.server_config()]).unwrap();

    assert!(
        matches!(catalog.invoker().invoke("paint", json!({})), ToolOutcome::ToolError(result) if result["isError"] == true)
    );
}

#[test]
fn invoker_classifies_unknown_tool_and_invalid_arguments_as_validation() {
    let server = FakeMcpServer::spawn(vec![
        (200, tool_list()),
        (
            200,
            json!({ "jsonrpc": "2.0", "id": 2, "error": { "code": -32602, "message": "color is required" } }),
        ),
    ]);
    let catalog = McpToolCatalog::discover("direct-api", &[server.server_config()]).unwrap();

    assert!(
        matches!(catalog.invoker().invoke("unknown", json!({})), ToolOutcome::Validation(message) if message.contains("unknown MCP tool"))
    );
    assert!(
        matches!(catalog.invoker().invoke("paint", json!({})), ToolOutcome::Validation(message) if message == "color is required")
    );
    assert_eq!(server.requests.lock().len(), 2);
}

#[test]
fn invoker_classifies_bad_mcp_envelopes_and_http_failures_as_transport_errors() {
    let malformed_server = FakeMcpServer::spawn(vec![
        (200, tool_list()),
        (200, json!({ "jsonrpc": "2.0", "id": 2 })),
    ]);
    let malformed_catalog =
        McpToolCatalog::discover("live", &[malformed_server.server_config()]).unwrap();
    assert!(matches!(
        malformed_catalog.invoker().invoke("paint", json!({})),
        ToolOutcome::Transport(_)
    ));

    let failed_server = FakeMcpServer::spawn(vec![
        (200, tool_list()),
        (400, json!({ "error": "bad request" })),
    ]);
    let failed_catalog =
        McpToolCatalog::discover("live", &[failed_server.server_config()]).unwrap();
    assert!(matches!(
        failed_catalog.invoker().invoke("paint", json!({})),
        ToolOutcome::Transport(_)
    ));
}
