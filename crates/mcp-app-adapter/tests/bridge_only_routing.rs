use lumvise_app_core::{AcquireResult, ActivationRequest, AppRuntimeCoordinator, OwnerLease};
use lumvise_mcp_app_adapter::{AppBridgeConfig, McpAppConfig, open_server};
use lumvise_mcp_core::{
    APP_BRIDGE_PROTOCOL_MAJOR, AppBridgeInvocationRequestV1, AppBridgeInvocationResponseV1,
    AppBridgeInvocationStatusV1,
};
use prost::Message;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

#[derive(Debug, PartialEq)]
struct BridgeRequest {
    request_line: String,
    body: Vec<u8>,
}

enum BridgeResponse {
    Json(Value),
    Protobuf(Vec<u8>),
}

struct FakeBridge {
    base_url: String,
    requests: Receiver<BridgeRequest>,
    server: JoinHandle<()>,
}

impl FakeBridge {
    fn start(responses: Vec<BridgeResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (request_sender, requests) = channel();
        let server = thread::spawn(move || serve_responses(listener, responses, request_sender));
        Self {
            base_url,
            requests,
            server,
        }
    }

    fn finish(self) -> Vec<BridgeRequest> {
        self.server.join().unwrap();
        self.requests.into_iter().collect()
    }
}

fn serve_responses(
    listener: TcpListener,
    responses: Vec<BridgeResponse>,
    request_sender: Sender<BridgeRequest>,
) {
    for response in responses {
        let (mut stream, _) = listener.accept().unwrap();
        request_sender.send(read_request(&stream)).unwrap();
        write_response(&mut stream, response);
    }
}

fn read_request(stream: &TcpStream) -> BridgeRequest {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let content_length = read_content_length(&mut reader);
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    BridgeRequest {
        request_line: request_line.trim_end().to_string(),
        body,
    }
}

fn read_content_length(reader: &mut impl BufRead) -> usize {
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            return content_length;
        }
        content_length = header_content_length(line).unwrap_or(content_length);
    }
}

fn header_content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    name.eq_ignore_ascii_case("content-length")
        .then(|| value.trim().parse().ok())
        .flatten()
}

fn write_response(stream: &mut TcpStream, response: BridgeResponse) {
    let (content_type, body) = match response {
        BridgeResponse::Json(value) => ("application/json", value.to_string().into_bytes()),
        BridgeResponse::Protobuf(bytes) => ("application/protobuf", bytes),
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

fn server_for_bridge(
    base_url: &str,
    runtime_root: &Path,
) -> (lumvise_mcp_core::LumviseMcpServer, OwnerLease) {
    let coordinator = AppRuntimeCoordinator::new(runtime_root, |_| {
        panic!("bridge fixture must acquire its in-process owner")
    });
    let mut owner = match coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("acquire bridge fixture Runtime")
    {
        AcquireResult::Owner(owner) => owner,
        AcquireResult::Forwarded(_) => panic!("bridge fixture unexpectedly forwarded"),
    };
    owner.mark_ready(base_url).expect("mark bridge ready");
    let bridge = AppBridgeConfig::discovery().with_runtime_root(runtime_root);
    (open_server(McpAppConfig::from_app_bridge(bridge)), owner)
}

fn plugin_surface() -> Value {
    json!({
        "generation": "fixture-generation-1",
        "freshness": "current",
        "capabilities": [{
            "plugin_id": "plugin.example",
            "capability_id": "run",
            "dynamic_tool_name": "app_plugin.plugin.example.run",
            "input_schema": { "type": "object" },
            "availability": "ready"
        }]
    })
}
fn assistant_plugin_surface() -> Value {
    json!({
        "generation": "assistant-public",
        "freshness": "current",
        "capabilities": [{
            "plugin_id": "builtin.assistant",
            "capability_id": "start_assistant_session",
            "dynamic_tool_name": "app_plugin.builtin.assistant.start_assistant_session",
            "description": "Start one caller-owned Assistant session.",
            "input_schema": {
                "type": "object",
                "properties": {"prompt": {"type": "string"}},
                "required": ["prompt"]
            },
            "availability": "ready"
        }]
    })
}

#[test]
fn native_mcp_start_selects_the_requested_session_driver() {
    for in_session in [true, false] {
        let started = AppBridgeInvocationResponseV1 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id: "1".into(),
            status: AppBridgeInvocationStatusV1::Completed as i32,
            output_json: serde_json::to_vec(
                &json!({"state":{"session_id":"selected","session_epoch":1},"active_tools":[]}),
            )
            .unwrap(),
            message: String::new(),
            retryable: false,
        };
        let bridge = FakeBridge::start(vec![
            BridgeResponse::Json(assistant_plugin_surface()),
            BridgeResponse::Protobuf(started.encode_to_vec()),
        ]);
        let root = tempfile::tempdir().unwrap();
        let (server, _owner) = server_for_bridge(&bridge.base_url, root.path());
        let result = response_value(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                "name":"app_plugin.builtin.assistant.start_assistant_session","arguments":{"prompt":"Discuss","in_session":in_session}
            }}),
        );
        assert!(result.get("error").is_none(), "{result}");
        let requests = bridge.finish();
        let request = AppBridgeInvocationRequestV1::decode(requests[1].body.as_slice()).unwrap();
        let input: Value = serde_json::from_slice(&request.input_json).unwrap();
        assert_eq!(input.get("native_llm_caller").is_some(), in_session);
        assert!(input.get("in_session").is_none());
    }
}

#[test]
fn discovery_exposes_background_context_answers_without_starting_a_session() {
    let mut surface = assistant_plugin_surface();
    for id in ["observe_assistant_state", "answer_spawner_question"] {
        surface["capabilities"].as_array_mut().unwrap().push(json!({
            "plugin_id":"builtin.assistant","capability_id":id,
            "dynamic_tool_name":format!("app_plugin.builtin.assistant.{id}"),
            "input_schema":{"type":"object"},"availability":"ready"
        }));
    }
    let bridge = FakeBridge::start(vec![
        BridgeResponse::Json(surface),
        BridgeResponse::Json(assistant_scoped_surface()),
    ]);
    let root = tempfile::tempdir().unwrap();
    let (server, _owner) = server_for_bridge(&bridge.base_url, root.path());
    let result = response_value(
        &server,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    let tools = result["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "app_plugin.builtin.assistant.answer_spawner_question")
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "app_plugin.builtin.assistant.observe_assistant_state")
    );
    let start = tools
        .iter()
        .find(|tool| tool["name"] == "app_plugin.builtin.assistant.start_assistant_session")
        .unwrap();
    assert!(
        start["description"]
            .as_str()
            .unwrap()
            .starts_with("Start one caller-owned Assistant session.")
    );
    assert_eq!(
        start["inputSchema"]["properties"]["in_session"]["default"],
        true
    );
    bridge.finish();
}

fn assistant_scoped_surface() -> Value {
    let capability = |name: &str| {
        json!({
            "plugin_id": "builtin.assistant",
            "capability_id": name,
            "dynamic_tool_name": name,
            "description": format!("Use {name} for the bound Assistant session."),
            "input_schema": {
                "type": "object",
                "properties": {
                    "plugin_id": {"type": "string"},
                    "session_id": {"type": "string"},
                    "session_epoch": {"type": "integer"},
                    "content": {"type": "string"},
                    "final": {"type": "boolean"},
                    "summary": {"type": "string"}
                },
                "required": ["plugin_id", "session_id"]
            },
            "availability": "ready"
        })
    };
    json!({
        "generation": "assistant-scoped",
        "freshness": "current",
        "capabilities": [
            capability("assistant.respond"),
            capability("assistant.await_turn"),
            capability("assistant.finish")
        ]
    })
}

fn response_value(server: &lumvise_mcp_core::LumviseMcpServer, request: Value) -> Value {
    let response = server
        .handle_json_line(&request.to_string())
        .unwrap()
        .unwrap();
    serde_json::from_str(&response).unwrap()
}

#[test]
fn tool_catalog_contains_only_bridge_meta_and_discovered_plugin_tools() {
    let fake_bridge = FakeBridge::start(vec![BridgeResponse::Json(plugin_surface())]);
    let workspace = tempfile::tempdir().unwrap();
    let (server, _owner) =
        server_for_bridge(&fake_bridge.base_url, &workspace.path().join("runtime"));

    let response = response_value(
        &server,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    );
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let _requests = fake_bridge.finish();

    assert_eq!(
        names,
        [
            "discover_app_plugins",
            "invoke_app_plugin_capability",
            "app_bridge_status",
            "app_plugin.plugin.example.run",
        ]
    );
}

#[test]
fn plugin_owned_guidance_refreshes_through_discovery_and_disappears_with_its_capability() {
    let mut original = plugin_surface();
    original["capabilities"][0]["description"] = json!("Read the saved scene before editing.");
    let mut revised = original.clone();
    revised["generation"] = json!("fixture-generation-2");
    revised["capabilities"][0]["description"] =
        json!("Read the saved scene; preserve user edits and read back after saving.");
    let removed =
        json!({"generation":"fixture-generation-3", "freshness":"current", "capabilities":[]});
    let bridge = FakeBridge::start(vec![
        BridgeResponse::Json(original.clone()),
        BridgeResponse::Json(revised.clone()),
        BridgeResponse::Json(revised.clone()),
        BridgeResponse::Json(removed),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let (server, _owner) = server_for_bridge(&bridge.base_url, workspace.path());
    assert_catalog_guidance(&server, &original["capabilities"][0]["description"]);
    assert_eq!(discovered_surface(&server), revised);
    assert_catalog_guidance(&server, &revised["capabilities"][0]["description"]);
    let empty = response_value(
        &server,
        json!({"jsonrpc":"2.0", "id":4, "method":"tools/list"}),
    );
    assert_eq!(empty["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(bridge.finish().len(), 4);
}

#[test]
fn discovery_marks_cached_guidance_stale_when_the_current_surface_cannot_be_read() {
    let mut original = plugin_surface();
    original["capabilities"][0]["description"] = json!("Read before saving a project diagram.");
    let bridge = FakeBridge::start(vec![
        BridgeResponse::Json(original.clone()),
        BridgeResponse::Json(json!({"capabilities":"unreadable"})),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let (server, _owner) = server_for_bridge(&bridge.base_url, workspace.path());
    assert_eq!(discovered_surface(&server), original);
    let stale = discovered_surface(&server);
    assert_eq!(stale["freshness"], "stale");
    assert_eq!(stale["capabilities"], original["capabilities"]);
    assert!(
        stale["catalog_error"]
            .as_str()
            .unwrap()
            .contains("expected versioned catalog")
    );
    assert_eq!(bridge.finish().len(), 2);
}

fn discovered_surface(server: &lumvise_mcp_core::LumviseMcpServer) -> Value {
    let response = response_value(
        server,
        json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"discover_app_plugins", "arguments":{}}}),
    );
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn installed_but_unavailable_plugins_keep_their_guidance_and_explicit_readiness() {
    let mut stopped = plugin_surface();
    stopped["capabilities"][0]["description"] = json!("Read before updating the current project.");
    stopped["capabilities"][0]["availability"] = json!("unavailable");
    let bridge = FakeBridge::start(vec![
        BridgeResponse::Json(stopped.clone()),
        BridgeResponse::Json(stopped.clone()),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let (server, _owner) = server_for_bridge(&bridge.base_url, workspace.path());
    assert_eq!(discovered_surface(&server), stopped);
    assert_catalog_guidance(&server, &stopped["capabilities"][0]["description"]);
    assert_eq!(bridge.finish().len(), 2);
}

fn assert_catalog_guidance(server: &lumvise_mcp_core::LumviseMcpServer, expected: &Value) {
    let catalog = response_value(
        server,
        json!({"jsonrpc":"2.0", "id":1, "method":"tools/list"}),
    );
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let plugin = tools
        .iter()
        .find(|tool| tool["name"] == "app_plugin.plugin.example.run")
        .unwrap();
    assert_eq!(&plugin["description"], expected);
    assert_eq!(plugin["inputSchema"], json!({"type":"object"}));
    let discovery = tools
        .iter()
        .find(|tool| tool["name"] == "discover_app_plugins")
        .unwrap();
    assert!(
        discovery["description"]
            .as_str()
            .unwrap()
            .contains("stale inventory")
    );
    assert!(
        discovery["description"]
            .as_str()
            .unwrap()
            .contains("availability=ready")
    );
}

#[test]
fn stable_assistant_catalog_routes_named_and_generic_calls_through_the_session_binding() {
    let completed = |output_json: &[u8]| AppBridgeInvocationResponseV1 {
        protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
        request_id: "fixture".into(),
        status: AppBridgeInvocationStatusV1::Completed as i32,
        output_json: output_json.to_vec(),
        message: String::new(),
        retryable: false,
    };
    let started = completed(
        br#"{"state":{"session_id":"logical-session","session_epoch":27},"session_instructions":"Use the bound tools.","active_tools":["assistant.respond","assistant.await_turn","assistant.finish"]}"#,
    );
    let responded =
        completed(br#"{"decision":"assistant_segment_accepted","playback":{"accepted":true}}"#);
    let fake_bridge = FakeBridge::start(vec![
        BridgeResponse::Json(assistant_plugin_surface()),
        BridgeResponse::Json(assistant_scoped_surface()),
        BridgeResponse::Protobuf(started.encode_to_vec()),
        BridgeResponse::Json(assistant_plugin_surface()),
        BridgeResponse::Json(assistant_scoped_surface()),
        BridgeResponse::Protobuf(responded.encode_to_vec()),
        BridgeResponse::Protobuf(responded.encode_to_vec()),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let coordinator = AppRuntimeCoordinator::new(workspace.path().join("runtime"), |_| {
        panic!("bridge fixture must acquire its in-process owner")
    });
    let mut owner = match coordinator
        .acquire_or_forward(ActivationRequest::default())
        .expect("acquire bridge fixture Runtime")
    {
        AcquireResult::Owner(owner) => owner,
        AcquireResult::Forwarded(_) => panic!("bridge fixture unexpectedly forwarded"),
    };
    owner
        .mark_ready(fake_bridge.base_url.clone())
        .expect("mark bridge ready");
    let server = open_server(
        McpAppConfig::from_app_bridge(
            AppBridgeConfig::discovery().with_runtime_root(workspace.path().join("runtime")),
        )
        .with_native_assistant_caller("configured-engine", "configured-broker"),
    );

    let initial_catalog = response_value(
        &server,
        json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
    );
    let started_output = response_value(
        &server,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "app_plugin.builtin.assistant.start_assistant_session",
                "arguments": {"prompt": "begin"}
            }
        }),
    );
    let catalog = response_value(
        &server,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    let response = response_value(
        &server,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "assistant.respond",
                "arguments": {"content": "First sentence.", "final": false, "turn_id":"logical-session:1:0"}
            }
        }),
    );
    let generic_response = response_value(
        &server,
        json!({"jsonrpc":"2.0","id":4,
        "method":"tools/call","params":{"name":"invoke_app_plugin_capability",
        "arguments":{"plugin_id":"builtin.assistant","capability_id":"assistant.respond",
        "input":{"content":"Second sentence.","turn_id":"logical-session:1:0","session_epoch":999,"final":true}}}}),
    );
    assert!(
        generic_response.get("error").is_none(),
        "{generic_response}"
    );
    assert_eq!(
        initial_catalog["result"]["tools"],
        catalog["result"]["tools"]
    );
    let requests = fake_bridge.finish();
    let start = AppBridgeInvocationRequestV1::decode(requests[2].body.as_slice()).unwrap();
    let respond = AppBridgeInvocationRequestV1::decode(requests[5].body.as_slice()).unwrap();
    let generic = AppBridgeInvocationRequestV1::decode(requests[6].body.as_slice()).unwrap();
    assert_eq!(generic.scope_id.as_deref(), Some("assistant_session"));
    assert_eq!(generic.session_id, "logical-session");
    assert_eq!(generic.capability_id, "assistant.respond");
    assert_eq!(
        serde_json::from_slice::<Value>(&generic.input_json).unwrap()["session_epoch"],
        27,
        "the bound epoch must override a caller-supplied epoch"
    );
    let names = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    let respond_schema = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "assistant.respond")
        .unwrap();

    assert_eq!(
        started_output["result"]["content"][0]["text"],
        serde_json::to_string(&json!({
            "state": {"session_id": "logical-session", "session_epoch": 27},
            "session_instructions": "Use the bound tools.",
            "active_tools": ["assistant.respond", "assistant.await_turn", "assistant.finish"]
        }))
        .unwrap()
    );
    assert!(response.get("error").is_none());
    assert_eq!(start.owner_id, "configured-broker");
    assert_eq!(start.plugin_id, "builtin.assistant");
    assert_eq!(start.capability_id, "start_assistant_session");
    assert_eq!(
        serde_json::from_slice::<Value>(&start.input_json).unwrap()["native_llm_caller"],
        json!({"engine": "configured-engine", "instance_id": "configured-broker"})
    );
    assert_eq!(
        names,
        [
            "discover_app_plugins",
            "invoke_app_plugin_capability",
            "app_bridge_status",
            "app_plugin.builtin.assistant.start_assistant_session",
            "assistant.respond",
            "assistant.await_turn",
            "assistant.finish",
        ]
    );
    assert!(
        respond_schema["inputSchema"]["properties"]
            .get("plugin_id")
            .is_none()
    );
    assert!(
        respond_schema["inputSchema"]["properties"]
            .get("session_id")
            .is_none()
    );
    assert_eq!(respond.scope_id.as_deref(), Some("assistant_session"));
    assert!(
        respond_schema["inputSchema"]["properties"]
            .get("session_epoch")
            .is_none()
    );
    assert_eq!(respond.session_id, "logical-session");
    assert_eq!(respond.capability_id, "assistant.respond");
    assert_eq!(
        serde_json::from_slice::<Value>(&respond.input_json).unwrap(),
        json!({
            "content": "First sentence.",
            "turn_id":"logical-session:1:0",
            "final": false,
            "plugin_id": "builtin.assistant",
            "session_id": "logical-session",
            "session_epoch": 27
        })
    );
}

#[test]
fn dynamic_plugin_invocation_uses_only_the_desktop_bridge_route() {
    let bridge_response = AppBridgeInvocationResponseV1 {
        protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
        request_id: "1".into(),
        status: AppBridgeInvocationStatusV1::Completed as i32,
        output_json: br#"{"ok":true}"#.to_vec(),
        message: String::new(),
        retryable: false,
    };
    let fake_bridge = FakeBridge::start(vec![
        BridgeResponse::Json(plugin_surface()),
        BridgeResponse::Protobuf(bridge_response.encode_to_vec()),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let (server, _owner) =
        server_for_bridge(&fake_bridge.base_url, &workspace.path().join("runtime"));

    response_value(
        &server,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "app_plugin.plugin.example.run",
                "arguments": { "input": { "value": 7 } }
            }
        }),
    );
    let requests = fake_bridge.finish();
    let invocation = AppBridgeInvocationRequestV1::decode(requests[1].body.as_slice()).unwrap();
    let request_lines = requests
        .into_iter()
        .map(|request| request.request_line)
        .collect::<Vec<_>>();

    assert_eq!(request_lines.len(), 2);
    assert!(request_lines[0].starts_with("GET /api/mcp/plugins/surface?credential="));
    assert!(request_lines[0].ends_with(" HTTP/1.1"));
    assert!(request_lines[1].starts_with("POST /api/mcp/plugins/invoke-v1?credential="));
    assert!(request_lines[1].ends_with(" HTTP/1.1"));
    assert_eq!(invocation.request_id, "1");
    assert_eq!(invocation.plugin_id, "plugin.example");
    assert_eq!(invocation.capability_id, "run");
    assert!(invocation.deadline_unix_ms > 0);
}

#[test]
fn bound_workspace_and_knowledge_tools_keep_their_plugin_ownership() {
    let completed = |output: Value| {
        BridgeResponse::Protobuf(
            AppBridgeInvocationResponseV1 {
                protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
                request_id: "fixture".into(),
                status: AppBridgeInvocationStatusV1::Completed as i32,
                output_json: serde_json::to_vec(&output).unwrap(),
                message: String::new(),
                retryable: false,
            }
            .encode_to_vec(),
        )
    };
    let scoped = json!({"generation":"workspace-tools", "capabilities":[
        {"plugin_id":"builtin.canvas", "capability_id":"workspace_canvas.navigate",
         "dynamic_tool_name":"workspace_canvas.navigate", "availability":"ready"},
        {"plugin_id":"builtin.knowledge", "capability_id":"knowledge.search",
         "dynamic_tool_name":"knowledge.search", "availability":"ready"}
    ]});
    let bridge = FakeBridge::start(vec![
        BridgeResponse::Json(assistant_plugin_surface()),
        completed(
            json!({"state":{"session_id":"workspace-session","session_epoch":1},"active_tools":[]}),
        ),
        BridgeResponse::Json(scoped.clone()),
        completed(json!({"accepted":true})),
        BridgeResponse::Json(scoped),
        completed(json!({"results":[]})),
    ]);
    let workspace = tempfile::tempdir().unwrap();
    let (server, _owner) = server_for_bridge(&bridge.base_url, &workspace.path().join("runtime"));
    let requests = [
        (
            "app_plugin.builtin.assistant.start_assistant_session",
            json!({"prompt":"Guide me"}),
        ),
        (
            "workspace_canvas.navigate",
            json!({"targetId":"record", "projectRoot":"/project", "targetKind":"artifact"}),
        ),
        (
            "invoke_app_plugin_capability",
            json!({"plugin_id":"builtin.knowledge", "capability_id":"knowledge.search", "input":{"query":"record"}}),
        ),
    ];
    for (id, (name, arguments)) in requests.into_iter().enumerate() {
        let result = response_value(
            &server,
            json!({"jsonrpc":"2.0", "id":id,
            "method":"tools/call", "params":{"name":name, "arguments":arguments}}),
        );
        assert!(result.get("error").is_none(), "{result}");
    }
    let observed = bridge.finish();
    for (index, plugin_id, capability_id) in [
        (3, "builtin.canvas", "workspace_canvas.navigate"),
        (5, "builtin.knowledge", "knowledge.search"),
    ] {
        let request =
            AppBridgeInvocationRequestV1::decode(observed[index].body.as_slice()).unwrap();
        assert_eq!(request.plugin_id, plugin_id);
        assert_eq!(request.capability_id, capability_id);
        assert_eq!(request.session_id, "workspace-session");
        assert_eq!(request.scope_id.as_deref(), Some("assistant_session"));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.input_json).unwrap()["plugin_id"],
            plugin_id
        );
    }
}
