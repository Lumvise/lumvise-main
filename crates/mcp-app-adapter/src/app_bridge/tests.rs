use super::*;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::thread;
#[test]
fn capability_tool_name_uses_canonical_name_when_surface_omits_dynamic_name() {
    let capability = PluginCapability {
        plugin_id: "plugin.example".to_string(),
        capability_id: "run".to_string(),
        dynamic_tool_name: None,
        description: None,
        input_schema: None,
        availability: None,
    };
    assert_eq!(
        capability_tool_name(&capability),
        "app_plugin.plugin.example.run"
    );
}

#[test]
fn cloned_config_observes_terminal_owner_shutdown_without_relaunch() {
    let workspace = tempfile::tempdir().expect("runtime workspace");
    let launcher_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let launcher_calls_for_coordinator = Arc::clone(&launcher_calls);
    let coordinator = Arc::new(AppRuntimeCoordinator::new(workspace.path(), move |_| {
        launcher_calls_for_coordinator.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    let config = AppBridgeConfig {
        discovery_path: workspace.path().join(RUNTIME_DISCOVERY_FILE),
        coordinator,
        terminal: Arc::new(AtomicBool::new(false)),
    };
    let clone = config.clone();
    let mut owner = match config
        .coordinator()
        .acquire_or_forward(lumvise_app_core::ActivationRequest::default())
        .expect("owner lease")
    {
        lumvise_app_core::AcquireResult::Owner(owner) => owner,
        lumvise_app_core::AcquireResult::Forwarded(_) => panic!("expected owner"),
    };
    let generation = owner
        .mark_ready("http://127.0.0.1:61238")
        .expect("ready generation");
    let observed = clone
        .current_connection_until(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .expect("clone observes generation");
    assert_eq!(observed.generation_nonce, generation.generation_nonce);
    owner
        .begin_quit(lumvise_app_core::QuitRequest::default())
        .expect("owner quit");

    let terminal = clone.current_connection_until(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        &AtomicBool::new(false),
    );
    assert!(
        terminal
            .expect_err("terminal coordinator must not relaunch")
            .contains("runtime is quitting")
    );
    assert!(clone.is_terminal());
    assert!(config.is_terminal());
    assert_eq!(launcher_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn discovery_uses_coordinator_canonical_root() {
    let config = AppBridgeConfig::discovery();
    let production_root = AppRuntimeCoordinator::production()
        .runtime_root()
        .to_path_buf();
    assert_eq!(config.runtime_root(), production_root);
    assert_eq!(
        config.runtime_root(),
        config.coordinator().runtime_root().to_path_buf()
    );
}

#[test]
fn custom_root_rebinds_discovery_and_coordinator_together() {
    let config = AppBridgeConfig::discovery().with_runtime_root("/tmp/lumvise-adapter-test");
    assert_eq!(
        config.runtime_root(),
        PathBuf::from("/tmp/lumvise-adapter-test")
    );
    assert_eq!(
        config.coordinator().runtime_root(),
        Path::new("/tmp/lumvise-adapter-test")
    );
}
#[test]
fn cancellation_uses_logical_scoped_target_then_interrupts_and_cleans_up() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake bridge");
    let base_url = format!("http://{}", listener.local_addr().expect("fake address"));
    let (requests_sender, requests_receiver) = channel();
    let server = thread::spawn(move || {
        for response in [
            AppBridgeCancellationResponseV2 {
                protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
                cancelled: true,
            }
            .encode_to_vec(),
            AppBridgeInvocationResponseV2 {
                protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
                request_id: "interrupted".into(),
                status: AppBridgeInvocationStatusV2::Completed as i32,
                output_json: br#"{}"#.to_vec(),
                message: String::new(),
                retryable: false,
            }
            .encode_to_vec(),
        ] {
            let (mut stream, _) = listener.accept().expect("fake bridge request");
            requests_sender
                .send(read_test_request(&stream))
                .expect("record fake request");
            write_test_protobuf_response(&mut stream, response);
        }
    });
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime =
        AppBridgeRuntime::start(AppBridgeConfig::discovery().with_runtime_root(workspace.path()));
    let context = McpInvocationContext::with_timeout(
        "parent-request",
        "parent-owner",
        "transport-session",
        None,
        std::time::Duration::from_secs(10),
    );
    runtime
        .active_targets
        .lock()
        .expect("active target lock")
        .insert(
            context_key(&context),
            ActiveTarget {
                plugin_id: "builtin.knowledge".into(),
                connection: RuntimeConnection {
                    generation_nonce: "generation".into(),
                    app_bridge_credential: "credential".into(),
                    app_bridge_credential_expires_unix_ms: u64::MAX,
                    app_bridge_base_url: base_url,
                },
                session_id: "logical-session".into(),
                owner_id: "builtin.assistant".into(),
                scope_id: Some("assistant_session".into()),
                scope_epoch: Some(27),
                interrupt_on_cancel: true,
            },
        );

    assert!(runtime.cancel_controlled(&context).expect("cancelled"));
    let requests = [
        requests_receiver.recv().unwrap(),
        requests_receiver.recv().unwrap(),
    ];
    server.join().expect("fake bridge exits");
    let cancellation = AppBridgeCancellationRequestV2::decode(requests[0].1.as_slice())
        .expect("decode cancellation");
    let interrupted = AppBridgeInvocationRequestV2::decode(requests[1].1.as_slice())
        .expect("decode terminal transition");

    assert!(
        requests[0]
            .0
            .starts_with("POST /api/mcp/plugins/cancel-v2?credential=")
    );
    assert_eq!(cancellation.request_id, "parent-request");
    assert_eq!(cancellation.owner_id, "builtin.assistant");
    assert_eq!(cancellation.session_id, "logical-session");
    assert_eq!(cancellation.scope_id.as_deref(), Some("assistant_session"));
    assert_eq!(cancellation.plugin_id, "builtin.knowledge");
    assert!(
        requests[1]
            .0
            .starts_with("POST /api/mcp/plugins/invoke-v2?credential=")
    );
    assert!(
        interrupted
            .request_id
            .starts_with("parent-request:interrupted:")
    );
    assert_eq!(interrupted.owner_id, "parent-owner");
    assert_eq!(interrupted.session_id, "logical-session");
    assert_eq!(interrupted.scope_id, None);
    assert_eq!(interrupted.scope_epoch, None);
    assert_eq!(interrupted.plugin_id, "builtin.assistant");
    assert_eq!(interrupted.capability_id, "advance_assistant_state");
    assert_eq!(
        serde_json::from_slice::<Value>(&interrupted.input_json).unwrap()["event"],
        "interrupted"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&interrupted.input_json).unwrap()["session_epoch"],
        27
    );
    assert!(runtime.active_target(&context).unwrap().is_none());
}

fn read_test_request(stream: &TcpStream) -> (String, Vec<u8>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone request stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read request line");
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read request header");
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().expect("content length");
            }
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("read request body");
    (request_line.trim_end().into(), body)
}

fn write_test_protobuf_response(stream: &mut TcpStream, body: Vec<u8>) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/protobuf\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .expect("write response headers");
    stream.write_all(&body).expect("write response body");
}
