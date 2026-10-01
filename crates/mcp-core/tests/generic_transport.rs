use std::sync::{Arc, Mutex};

use lumvise_mcp_core::{LumviseMcpServer, McpApplication, McpApplicationError, McpTool};
use serde_json::{Value, json};

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
