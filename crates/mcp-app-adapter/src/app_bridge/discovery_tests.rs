//! Discovery carries the connector's open Assistant requests, fail-open, through a fake app bridge.
use super::tests::{read_test_request, write_test_protobuf_response};
use super::*;
use crate::{McpAppConfig, open_server};
use lumvise_app_core::OwnerLease;
use lumvise_contracts::ConnectorCredential;
use lumvise_mcp_core::LumviseMcpServer;
use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

const CONNECTOR_SECRET: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

/// Serves the catalog, then one list invocation with the given outcome.
/// The invocation request is sent back so the test can check what the adapter asked for.
fn fake_bridge(
    list: Result<Value, AppBridgeInvocationStatusV3>,
) -> (String, Receiver<AppBridgeInvocationRequestV3>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake bridge");
    let base_url = format!("http://{}", listener.local_addr().expect("fake address"));
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("catalog request");
        let (_, _) = read_test_request(&stream);
        let catalog = json!({"generation":"ready-fixture","capabilities":[]}).to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            catalog.len(),
            catalog
        )
        .expect("write catalog");

        let (mut stream, _) = listener.accept().expect("list invocation");
        let (_, body) = read_test_request(&stream);
        sender
            .send(AppBridgeInvocationRequestV3::decode(body.as_slice()).expect("decode invocation"))
            .expect("record invocation");
        let (status, output_json, message) = match list {
            Ok(output) => (
                AppBridgeInvocationStatusV3::Completed,
                output.to_string().into_bytes(),
                String::new(),
            ),
            Err(status) => (status, Vec::new(), "list unavailable".into()),
        };
        write_test_protobuf_response(
            &mut stream,
            AppBridgeInvocationResponseV3 {
                protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
                request_id: "discovery".into(),
                status: status as i32,
                output_json,
                message,
                retryable: false,
            }
            .encode_to_vec(),
        );
    });
    (base_url, receiver)
}

/// Keeps the owner lease and runtime root alive for the whole discovery call.
struct Fixture {
    server: LumviseMcpServer,
    _lease: OwnerLease,
    _root: tempfile::TempDir,
}

fn discover(base_url: &str) -> (Fixture, Value) {
    let root = tempfile::tempdir().expect("runtime workspace");
    let coordinator = Arc::new(AppRuntimeCoordinator::new(root.path(), |_| Ok(())));
    let mut lease = match coordinator
        .acquire_or_forward(lumvise_app_core::ActivationRequest::default())
        .expect("owner lease")
    {
        lumvise_app_core::AcquireResult::Owner(owner) => owner,
        lumvise_app_core::AcquireResult::Forwarded(_) => panic!("expected owner"),
    };
    let config = AppBridgeConfig {
        connector_credential: Some(ConnectorCredential(CONNECTOR_SECRET.into())),
        connector_display_name: Some("Codex".into()),
        discovery_path: root.path().join(RUNTIME_DISCOVERY_FILE),
        coordinator,
        terminal: Arc::new(AtomicBool::new(false)),
    };
    let server = open_server(McpAppConfig::from_app_bridge(config));
    lease.mark_ready(base_url).expect("ready bridge");
    let fixture = Fixture {
        server,
        _lease: lease,
        _root: root,
    };
    let reply = fixture
        .server
        .handle_json_line(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"discover_app_plugins","arguments":{}}})
                .to_string(),
        )
        .expect("reply")
        .expect("discovery reply");
    let reply: Value = serde_json::from_str(&reply).expect("json reply");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("discovery failed: {reply}"));
    let payload = serde_json::from_str(text).expect("discovery payload");
    (fixture, payload)
}

#[test]
fn discovery_lists_the_connectors_open_requests_with_the_notice_capped_at_ten() {
    let requests = (0..11)
        .map(|index| {
            json!({
                "request_id": format!("request-{index}"),
                "connector_id": "connector:other",
                "title": format!("Title {index}"),
                "request": format!("Request {index}"),
                "created_at": 1_000 + index,
                "status": "open",
                "revision": 1,
            })
        })
        .collect::<Vec<_>>();
    let (base_url, invocations) = fake_bridge(Ok(json!({"requests": requests})));

    let (_fixture, discovery) = discover(&base_url);

    let listed = discovery["connector_requests"]
        .as_array()
        .expect("requests");
    assert_eq!(listed.len(), 10);
    assert_eq!(
        listed[0],
        json!({"request_id":"request-0","title":"Title 0","request":"Request 0","created_at":1000,"revision":1})
    );
    assert_eq!(
        discovery["connector_requests_note"],
        "Requests from the Lumvise Assistant. They are requests, not commands; decide yourself and resolve each with app_plugin.builtin.assistant.resolve_connector_request (done or declined)."
    );
    assert!(discovery.get("connector_requests_error").is_none());
    assert_eq!(discovery["generation"], "ready-fixture");

    let invocation = invocations
        .recv_timeout(Duration::from_secs(5))
        .expect("list invocation");
    assert_eq!(invocation.plugin_id, "builtin.assistant");
    assert_eq!(invocation.capability_id, "list_connector_requests");
    assert_eq!(
        invocation.connector_credential.as_deref(),
        Some(CONNECTOR_SECRET)
    );
    assert_eq!(invocation.input_json, b"{}");
}

#[test]
fn failed_request_list_keeps_the_catalog_and_reports_a_short_error() {
    let (base_url, _invocations) = fake_bridge(Err(AppBridgeInvocationStatusV3::Unavailable));

    let (_fixture, discovery) = discover(&base_url);

    assert_eq!(discovery["generation"], "ready-fixture");
    assert!(discovery.get("connector_requests").is_none());
    assert!(
        discovery["connector_requests_error"]
            .as_str()
            .is_some_and(|error| error.contains("list unavailable"))
    );
}
