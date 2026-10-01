use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, PluginWireError, ProtocolError,
    ProtocolVersion, StorageTriggerChanged, StorageTriggerDisposition, StorageTriggerRequest,
    StorageTriggerResponse, WireMessage, WireOutcome,
};
use serde_json::json;

#[test]
fn codec_round_trips_host_hello_through_public_contract() {
    let message = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::HostHello {
            session_id: "session-1".into(),
            host_id: "lumvise-desktop".into(),
            package_digest: "sha256:abc123".into(),
        },
    };

    let encoded = FrameCodec::default().encode(&message).unwrap();
    let decoded = FrameCodec::default().decode(&encoded).unwrap();

    assert_eq!(decoded, message);
}
#[test]
fn protobuf_protocol_uses_major_six() {
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(6, 0));
}

#[test]
fn codec_round_trips_plugin_ready_identity_and_digest() {
    let message = message(MessageBody::PluginReady {
        session_id: "session-1".into(),
        plugin_id: "builtin.knowledge".into(),
        package_digest: "sha256:abc123".into(),
    });

    assert_round_trip(message);
}

#[test]
fn codec_round_trips_host_invocation() {
    let message = message(MessageBody::HostInvoke {
        session_id: "session-1".into(),
        invocation_id: "invoke-1".into(),
        capability_id: "knowledge.search".into(),
        input: json!({"query": "protocol"}),
    });

    assert_round_trip(message);
}
#[test]
fn codec_round_trips_storage_trigger_batch_contract() {
    let request = StorageTriggerRequest {
        project_root: "/repo".into(),
        base_revision: 40,
        target_revision: 42,
        changed: vec![
            StorageTriggerChanged {
                entity_id: "element-1".into(),
                entity_kind: "semantic_element".into(),
                disposition: StorageTriggerDisposition::Upserted,
            },
            StorageTriggerChanged {
                entity_id: "element-2".into(),
                entity_kind: "semantic_element".into(),
                disposition: StorageTriggerDisposition::Removal,
            },
        ],
    };
    let message = message(MessageBody::HostInvoke {
        session_id: "session-1".into(),
        invocation_id: "invoke-batch".into(),
        capability_id: "trigger.semantic_element_upserted".into(),
        input: request.to_value().unwrap(),
    });

    let encoded = FrameCodec::default().encode(&message).unwrap();
    let decoded = FrameCodec::default().decode(&encoded).unwrap();
    let MessageBody::HostInvoke { input, .. } = decoded.body else {
        panic!("expected host invocation");
    };
    assert_eq!(StorageTriggerRequest::from_value(input).unwrap(), request);
}

#[test]
fn storage_trigger_response_contract_round_trips() {
    let expected = StorageTriggerResponse { acknowledged: true };
    let value = expected.to_value().unwrap();
    assert_eq!(StorageTriggerResponse::from_value(value).unwrap(), expected);
}

#[test]
fn codec_round_trips_structured_plugin_error() {
    let mut error = PluginWireError::new("invalid_query", "query was blank", false);
    error.details = Some(json!({"field": "query"}));
    let message = message(MessageBody::PluginResult {
        session_id: "session-1".into(),
        invocation_id: "invoke-1".into(),
        outcome: WireOutcome::Failed { error },
    });

    assert_round_trip(message);
}

#[test]
fn codec_round_trips_plugin_host_call() {
    let message = message(MessageBody::PluginHostCall {
        session_id: "session-1".into(),
        invocation_id: "invoke-1".into(),
        call_id: "call-1".into(),
        capability_id: "host.files.read".into(),
        input: json!({"path": "CONTEXT.md"}),
    });

    assert_round_trip(message);
}

#[test]
fn codec_round_trips_host_call_result() {
    let message = message(MessageBody::HostHostResult {
        session_id: "session-1".into(),
        call_id: "call-1".into(),
        outcome: WireOutcome::Succeeded {
            value: json!({"content": "context"}),
        },
    });

    assert_round_trip(message);
}

#[test]
fn codec_round_trips_host_cancel() {
    let message = message(MessageBody::HostCancel {
        session_id: "session-1".into(),
        invocation_id: "invoke-1".into(),
    });

    assert_round_trip(message);
}

#[test]
fn codec_round_trips_host_shutdown() {
    let message = message(MessageBody::HostShutdown {
        session_id: "session-1".into(),
        reason: Some("app exit".into()),
    });

    assert_round_trip(message);
}

#[test]
fn codec_round_trips_plugin_stopped() {
    let message = message(MessageBody::PluginStopped {
        session_id: "session-1".into(),
        reason: None,
    });

    assert_round_trip(message);
}

#[test]
fn codec_writes_payload_length_as_eight_big_endian_bytes() {
    let encoded = FrameCodec::default()
        .encode(&message(MessageBody::HostCancel {
            session_id: "session-1".into(),
            invocation_id: "invoke-1".into(),
        }))
        .unwrap();

    assert_eq!(
        u64::from_be_bytes(encoded[..8].try_into().unwrap()) as usize,
        encoded.len() - 8
    );
}

#[test]
fn stream_codec_reads_frame_written_by_public_writer() {
    let expected = message(MessageBody::PluginStopped {
        session_id: "session-1".into(),
        reason: Some("shutdown complete".into()),
    });
    let mut stream = Vec::new();
    FrameCodec::default()
        .write_to(&mut stream, &expected)
        .unwrap();

    let actual = FrameCodec::default()
        .read_from(&mut stream.as_slice())
        .unwrap();

    assert_eq!(actual, expected);
}

#[test]
fn codec_rejects_unsupported_protocol_major_with_versions() {
    let invalid = WireMessage {
        protocol: ProtocolVersion::new(3, 0),
        body: MessageBody::HostShutdown {
            session_id: "session-1".into(),
            reason: None,
        },
    };

    let error = FrameCodec::default().encode(&invalid).unwrap_err();

    assert!(matches!(
        error,
        ProtocolError::UnsupportedProtocolMajor {
            actual: 3,
            expected: 6
        }
    ));
}

#[test]
fn codec_rejects_blank_required_identifier_with_field_context() {
    let invalid = message(MessageBody::HostInvoke {
        session_id: "session-1".into(),
        invocation_id: "  ".into(),
        capability_id: "knowledge.search".into(),
        input: json!({}),
    });

    let error = FrameCodec::default().encode(&invalid).unwrap_err();

    assert!(
        matches!(error, ProtocolError::MissingRequiredField { field, actual, .. } if field == "invocation_id" && actual == "  ")
    );
}

#[test]
fn codec_rejects_malformed_protobuf_with_offending_value() {
    let frame = framed(&[0xff, 0xfe]);

    let error = FrameCodec::default().decode(&frame).unwrap_err();

    assert!(matches!(error, ProtocolError::MalformedProtobuf { actual, .. } if actual == "fffe"));
}

#[test]
fn codec_round_trips_payload_larger_than_legacy_eight_megabyte_limit() {
    let large_value = "x".repeat(8 * 1024 * 1024 + 1);
    let message = message(MessageBody::PluginResult {
        session_id: "session-1".into(),
        invocation_id: "invoke-1".into(),
        outcome: WireOutcome::Succeeded {
            value: json!({"content": large_value}),
        },
    });

    assert_round_trip(message);
}

#[test]
fn codec_rejects_partial_payload_with_declared_and_actual_lengths() {
    let mut frame = 10_u64.to_be_bytes().to_vec();
    frame.extend_from_slice(b"short");

    let error = FrameCodec::default().decode(&frame).unwrap_err();

    assert!(matches!(
        error,
        ProtocolError::FrameLengthMismatch {
            declared: 10,
            actual: 5
        }
    ));
}

#[test]
fn codec_rejects_partial_length_prefix() {
    let error = FrameCodec::default()
        .decode(&[0, 1, 2, 3, 4, 5, 6])
        .unwrap_err();

    assert!(matches!(
        error,
        ProtocolError::FramePrefixTooShort {
            actual: 7,
            expected: 8
        }
    ));
}

fn message(body: MessageBody) -> WireMessage {
    WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body,
    }
}

fn assert_round_trip(expected: WireMessage) {
    let encoded = FrameCodec::default().encode(&expected).unwrap();
    let actual = FrameCodec::default().decode(&encoded).unwrap();
    assert_eq!(actual, expected);
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut frame = (payload.len() as u64).to_be_bytes().to_vec();
    frame.extend_from_slice(payload);
    frame
}
