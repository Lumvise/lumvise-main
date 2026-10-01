use std::{
    io::{Cursor, Write},
    os::unix::net::UnixStream,
    thread,
    time::Duration,
};

use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, WireMessage, WireOutcome,
};
use lumvise_plugin_sdk::{PluginApplication, PluginContext, PluginError, SdkError, run};
use serde_json::{Value, json};

struct ManifestPlugin;

struct HostCallingPlugin;

#[derive(Default)]
struct FlushGateWriter {
    committed: Vec<u8>,
    pending: Vec<u8>,
    flush_count: usize,
}

impl Write for FlushGateWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.committed.append(&mut self.pending);
        self.flush_count += 1;
        Ok(())
    }
}

impl PluginApplication for ManifestPlugin {
    fn plugin_id(&self) -> &str {
        "builtin.knowledge"
    }

    fn dispatch(
        &self,
        capability_id: &str,
        _input: Value,
        _context: &mut PluginContext<'_>,
    ) -> Result<Value, PluginError> {
        match capability_id {
            "knowledge.manifest" => Ok(json!({"name": "Knowledge"})),
            other => Err(PluginError::unknown_capability(other)),
        }
    }
}

impl PluginApplication for HostCallingPlugin {
    fn plugin_id(&self) -> &str {
        "host-calling.plugin"
    }

    fn dispatch(
        &self,
        capability_id: &str,
        input: Value,
        context: &mut PluginContext<'_>,
    ) -> Result<Value, PluginError> {
        match capability_id {
            "proxy.echo" => context.host_call("host.echo", input),
            "foreground.echo" => Ok(json!({"foreground": true})),
            other => Err(PluginError::unknown_capability(other)),
        }
    }
}

#[test]
fn run_hides_handshake_dispatch_and_shutdown_from_plugin_application() {
    let input = host_frames([
        MessageBody::HostHello {
            session_id: "session-1".into(),
            host_id: "lumvise-desktop".into(),
            package_digest: "sha256:abc123".into(),
        },
        MessageBody::HostInvoke {
            session_id: "session-1".into(),
            invocation_id: "invoke-1".into(),
            capability_id: "knowledge.manifest".into(),
            input: json!({}),
        },
        MessageBody::HostShutdown {
            session_id: "session-1".into(),
            reason: Some("test complete".into()),
        },
    ]);
    let mut output = Vec::new();

    run(&mut input.as_slice(), &mut output, &ManifestPlugin).unwrap();

    assert_eq!(decode_frames(&output, 3), expected_plugin_frames());
}

#[test]
fn run_flushes_each_frame_for_cross_process_visibility() {
    let input = host_frames([hello(), invoke("knowledge.manifest", json!({})), shutdown()]);
    let mut output = FlushGateWriter::default();

    run(&mut input.as_slice(), &mut output, &ManifestPlugin).unwrap();

    assert_eq!(output.flush_count, 3);
}

#[test]
fn run_rejects_invocation_from_another_session() {
    let input = host_frames([
        hello(),
        MessageBody::HostInvoke {
            session_id: "session-other".into(),
            invocation_id: "invoke-1".into(),
            capability_id: "knowledge.manifest".into(),
            input: json!({}),
        },
    ]);

    let error = run(&mut input.as_slice(), &mut Vec::new(), &ManifestPlugin).unwrap_err();

    assert!(
        matches!(error, SdkError::InvalidSession { actual, expected } if actual == "session-other" && expected == "session-1")
    );
}

#[test]
fn unknown_capability_becomes_structured_plugin_result() {
    let input = host_frames([hello(), invoke("missing.export", json!({})), shutdown()]);
    let mut output = Vec::new();

    run(&mut input.as_slice(), &mut output, &ManifestPlugin).unwrap();
    let messages = decode_frames(&output, 3);

    assert!(
        matches!(&messages[1].body, MessageBody::PluginResult { outcome: WireOutcome::Failed { error }, .. } if error.code == "unknown_capability" && error.message.contains("missing.export"))
    );
}

#[test]
fn host_call_rejects_mismatched_call_correlation_as_structured_failure() {
    let (mut host, plugin) = UnixStream::pair().expect("duplex stream");
    let mut plugin_reader = plugin.try_clone().expect("clone plugin reader");
    let runtime = thread::spawn(move || {
        let mut plugin_writer = plugin;
        run(&mut plugin_reader, &mut plugin_writer, &HostCallingPlugin)
    });
    let codec = FrameCodec::default();

    send_host_message(&mut host, hello());
    let _ = codec.read_from(&mut host).expect("plugin ready");
    send_host_message(&mut host, invoke("proxy.echo", json!({"text": "hello"})));
    let _ = codec.read_from(&mut host).expect("host call");
    send_host_message(
        &mut host,
        MessageBody::HostHostResult {
            session_id: "session-1".into(),
            call_id: "wrong-call".into(),
            outcome: WireOutcome::Succeeded {
                value: json!({"text": "hello"}),
            },
        },
    );
    drop(host);
    assert!(matches!(
        runtime
            .join()
            .expect("plugin session thread")
            .expect_err("wrong call id must terminate the session"),
        SdkError::UnexpectedMessage {
            actual: "host.host_result",
            expected: "a pending matching host call",
        }
    ));
}

#[test]
fn multiplexes_foreground_invocation_while_another_waits_for_a_host_result() {
    let (mut host, plugin) = UnixStream::pair().expect("duplex stream");
    let mut plugin_reader = plugin.try_clone().expect("clone plugin reader");
    let runtime = thread::spawn(move || {
        let mut plugin_writer = plugin;
        run(&mut plugin_reader, &mut plugin_writer, &HostCallingPlugin)
    });
    let codec = FrameCodec::default();

    send_host_message(&mut host, hello());
    assert!(matches!(
        codec.read_from(&mut host).expect("plugin ready").body,
        MessageBody::PluginReady { .. }
    ));
    send_host_message(
        &mut host,
        MessageBody::HostInvoke {
            session_id: "session-1".into(),
            invocation_id: "background".into(),
            capability_id: "proxy.echo".into(),
            input: json!({"value": "await host"}),
        },
    );
    assert!(matches!(
        codec.read_from(&mut host).expect("background host call").body,
        MessageBody::PluginHostCall { invocation_id, call_id, .. }
            if invocation_id == "background" && call_id == "background:host-call:1"
    ));

    send_host_message(
        &mut host,
        MessageBody::HostInvoke {
            session_id: "session-1".into(),
            invocation_id: "foreground".into(),
            capability_id: "foreground.echo".into(),
            input: json!({}),
        },
    );
    host.set_read_timeout(Some(Duration::from_secs(1)))
        .expect("foreground read timeout");
    assert!(matches!(
        codec.read_from(&mut host).expect("foreground result").body,
        MessageBody::PluginResult { invocation_id, outcome: WireOutcome::Succeeded { value }, .. }
            if invocation_id == "foreground" && value == json!({"foreground": true})
    ));

    send_host_message(
        &mut host,
        MessageBody::HostHostResult {
            session_id: "session-1".into(),
            call_id: "background:host-call:1".into(),
            outcome: WireOutcome::Succeeded {
                value: json!({"value": "host completed"}),
            },
        },
    );
    assert!(matches!(
        codec.read_from(&mut host).expect("background result").body,
        MessageBody::PluginResult { invocation_id, outcome: WireOutcome::Succeeded { value }, .. }
            if invocation_id == "background" && value == json!({"value": "host completed"})
    ));
    send_host_message(&mut host, shutdown());
    assert!(matches!(
        codec.read_from(&mut host).expect("plugin stopped").body,
        MessageBody::PluginStopped { .. }
    ));
    runtime
        .join()
        .expect("plugin session thread")
        .expect("plugin session completes");
}

#[test]
fn run_rejects_degraded_protocol_major_before_application_dispatch() {
    let mut input = encode_body(hello());
    let current_major =
        u8::try_from(CURRENT_PROTOCOL_VERSION.major).expect("current major fits one-byte varint");
    assert_eq!(&input[8..10], &[0x08, current_major]);
    let unsupported_major = CURRENT_PROTOCOL_VERSION
        .major
        .checked_add(1)
        .expect("unsupported major does not overflow");
    input[9] = u8::try_from(unsupported_major).expect("unsupported major fits one-byte varint");

    let error = run(&mut input.as_slice(), &mut Vec::new(), &ManifestPlugin).unwrap_err();

    assert!(matches!(
        error,
        SdkError::Protocol(
            lumvise_plugin_protocol::ProtocolError::UnsupportedProtocolMajor {
                actual,
                expected,
            }
        ) if actual == unsupported_major && expected == CURRENT_PROTOCOL_VERSION.major
    ));
}

fn send_host_message(stream: &mut UnixStream, body: MessageBody) {
    FrameCodec::default()
        .write_to(stream, &wire(body))
        .expect("write host frame");
}

fn host_frames<const N: usize>(bodies: [MessageBody; N]) -> Vec<u8> {
    bodies.into_iter().flat_map(encode_body).collect()
}

fn encode_body(body: MessageBody) -> Vec<u8> {
    FrameCodec::default()
        .encode(&WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body,
        })
        .unwrap()
}

fn decode_frames(bytes: &[u8], count: usize) -> Vec<WireMessage> {
    let mut cursor = Cursor::new(bytes);
    let codec = FrameCodec::default();
    (0..count)
        .map(|_| codec.read_from(&mut cursor).unwrap())
        .collect()
}

fn hello() -> MessageBody {
    MessageBody::HostHello {
        session_id: "session-1".into(),
        host_id: "lumvise-desktop".into(),
        package_digest: "sha256:abc123".into(),
    }
}

fn invoke(capability_id: &str, input: Value) -> MessageBody {
    MessageBody::HostInvoke {
        session_id: "session-1".into(),
        invocation_id: "invoke-1".into(),
        capability_id: capability_id.into(),
        input,
    }
}

fn shutdown() -> MessageBody {
    MessageBody::HostShutdown {
        session_id: "session-1".into(),
        reason: Some("test complete".into()),
    }
}

fn expected_plugin_frames() -> Vec<WireMessage> {
    vec![
        wire(MessageBody::PluginReady {
            session_id: "session-1".into(),
            plugin_id: "builtin.knowledge".into(),
            package_digest: "sha256:abc123".into(),
        }),
        wire(MessageBody::PluginResult {
            session_id: "session-1".into(),
            invocation_id: "invoke-1".into(),
            outcome: WireOutcome::Succeeded {
                value: json!({"name": "Knowledge"}),
            },
        }),
        wire(MessageBody::PluginStopped {
            session_id: "session-1".into(),
            reason: Some("test complete".into()),
        }),
    ]
}

fn wire(body: MessageBody) -> WireMessage {
    WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body,
    }
}
