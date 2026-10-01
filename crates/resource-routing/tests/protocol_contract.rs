use lumvise_resource_routing::protocol::{
    InvocationCancelV1, InvocationEnvelopeV1, InvocationStartV1, InvocationTerminalStatusV1,
    InvocationTerminalV1, LlmCompleteV1, LlmResultV1, LlmStreamEventV1, LlmStreamV1,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, PersistenceResultV1, ProtocolError, ReadinessNegotiationError,
    ReadinessRequestV1, RelationalOperationV1, ReverseMcpCallV1, ReverseMcpResultV1,
    SemanticOperationV1, SequenceValidator, SpeechToTextResultV1, SpeechToTextStreamV1,
    SpeechToTextV1, TextToSpeechResultV1, TextToSpeechStreamV1, TextToSpeechV1, TypedBinaryChunkV1,
    decode_frame, encode_frame, invocation_envelope_v1::Payload, invocation_start_v1::Operation,
    invocation_terminal_v1::Result as TerminalResult, llm_stream_event_v1::Event, negotiate_minor,
};

fn chunk() -> TypedBinaryChunkV1 {
    TypedBinaryChunkV1 {
        type_name: "application/octet-stream".into(),
        bytes: vec![1, 2, 3],
        metadata_json: None,
        final_chunk: true,
    }
}

fn start(request_id: &str, sequence: u64, operation: Operation) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id: request_id.into(),
        sequence,
        payload: Some(Payload::Start(InvocationStartV1 {
            deadline_unix_ms: 1,
            client_instance_id: "desktop-1".into(),
            operation: Some(operation),
        })),
    }
}

fn llm() -> LlmCompleteV1 {
    LlmCompleteV1 {
        provider_id: "provider".into(),
        model_override: Some("model".into()),
        conversation_id: Some("conversation".into()),
        provider_session_id: Some("session".into()),
        input: vec![chunk()],
        scoped_mcp_route_ids: vec!["route-1".into()],
    }
}

#[test]
fn surviving_operation_event_result_and_control_payloads_round_trip() {
    let operations = vec![
        Operation::LlmComplete(llm()),
        Operation::LlmStream(LlmStreamV1 {
            request: Some(llm()),
        }),
        Operation::SpeechToText(SpeechToTextV1 {
            audio: vec![0, 1],
            media_type: "audio/wav".into(),
            model: Some("stt".into()),
        }),
        Operation::SpeechToTextStream(SpeechToTextStreamV1 {
            request: Some(SpeechToTextV1 {
                audio: vec![2],
                media_type: "audio/pcm".into(),
                model: None,
            }),
        }),
        Operation::TextToSpeech(TextToSpeechV1 {
            text: "hello".into(),
            voice: "voice".into(),
            model: Some("tts".into()),
        }),
        Operation::TextToSpeechStream(TextToSpeechStreamV1 {
            request: Some(TextToSpeechV1 {
                text: "hello".into(),
                voice: "voice".into(),
                model: None,
            }),
        }),
        Operation::SemanticOperation(SemanticOperationV1 {
            operation_name: "SyncStructure".into(),
            records: vec![chunk()],
        }),
        Operation::RelationalOperation(RelationalOperationV1 {
            operation_name: "ApplyMutations".into(),
            records: vec![chunk()],
        }),
        Operation::Readiness(ReadinessRequestV1 {
            supported_majors: vec![PROTOCOL_MAJOR],
            supported_minors: vec![PROTOCOL_MINOR],
            deadline_unix_ms: 1,
            client_instance_id: "client".into(),
            requested_capabilities: vec![0],
        }),
    ];
    let mut envelopes = operations
        .into_iter()
        .enumerate()
        .map(|(sequence, operation)| start("request-1", sequence as u64, operation))
        .collect::<Vec<_>>();
    envelopes.extend([
        InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "request-1".into(),
            sequence: 10,
            payload: Some(Payload::BinaryChunk(chunk())),
        },
        InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "request-1".into(),
            sequence: 11,
            payload: Some(Payload::LlmStreamEvent(LlmStreamEventV1 {
                event_sequence: 0,
                event: Some(Event::TextDelta("delta".into())),
            })),
        },
        InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "request-1".into(),
            sequence: 12,
            payload: Some(Payload::ReverseMcpCall(ReverseMcpCallV1 {
                route_id: "route-1".into(),
                method: "tools/call".into(),
                parameters: vec![chunk()],
            })),
        },
        InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "request-1".into(),
            sequence: 13,
            payload: Some(Payload::ReverseMcpResult(ReverseMcpResultV1 {
                route_id: "route-1".into(),
                result: vec![chunk()],
                error_code: None,
            })),
        },
        InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "request-1".into(),
            sequence: 14,
            payload: Some(Payload::Cancel(InvocationCancelV1 {
                client_instance_id: "desktop-1".into(),
                reason: Some("cancel".into()),
            })),
        },
    ]);
    for envelope in envelopes {
        let frame = encode_frame(&envelope);
        assert_eq!(
            decode_frame::<InvocationEnvelopeV1>(&frame).unwrap(),
            envelope
        );
    }

    for terminal in [
        TerminalResult::Llm(LlmResultV1 {
            provider_session_id: Some("session".into()),
            output: vec![chunk()],
        }),
        TerminalResult::SpeechToText(SpeechToTextResultV1 {
            transcript: "text".into(),
            language: Some("en".into()),
            confidence: Some(0.9),
            segments: vec![chunk()],
        }),
        TerminalResult::TextToSpeech(TextToSpeechResultV1 {
            audio: vec![1],
            media_type: "audio/wav".into(),
            sample_rate: Some(16_000),
            metadata_json: None,
        }),
        TerminalResult::Semantic(PersistenceResultV1 {
            operation_name: "SyncStructure".into(),
            records: vec![chunk()],
        }),
        TerminalResult::Relational(PersistenceResultV1 {
            operation_name: "ApplyMutations".into(),
            records: vec![chunk()],
        }),
    ] {
        let envelope = InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            request_id: "terminal".into(),
            sequence: 0,
            payload: Some(Payload::Terminal(InvocationTerminalV1 {
                status: InvocationTerminalStatusV1::Completed as i32,
                retryable: false,
                outcome_unknown: false,
                error_code: None,
                message: None,
                result: Some(terminal),
            })),
        };
        assert_eq!(
            decode_frame::<InvocationEnvelopeV1>(&encode_frame(&envelope)).unwrap(),
            envelope
        );
    }
}

#[test]
fn typed_binary_frame_round_trips_large_payload() {
    let binary = vec![0xA5; 8 * 1024 * 1024 + 1];
    let envelope = InvocationEnvelopeV1 {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id: "large-binary".into(),
        sequence: 0,
        payload: Some(Payload::BinaryChunk(TypedBinaryChunkV1 {
            type_name: "audio/wav".into(),
            bytes: binary.clone(),
            metadata_json: None,
            final_chunk: true,
        })),
    };
    let frame = encode_frame(&envelope);
    assert_eq!(
        u64::from_be_bytes(frame[..8].try_into().unwrap()) as usize,
        frame.len() - 8
    );
    let Payload::BinaryChunk(chunk) = decode_frame::<InvocationEnvelopeV1>(&frame)
        .unwrap()
        .payload
        .unwrap()
    else {
        panic!("binary payload expected")
    };
    assert_eq!(chunk.bytes, binary);
}

#[test]
fn validator_rejects_wrong_major_and_bad_sequence() {
    let mut validator = SequenceValidator::new("request-1");
    let mut wrong_major = start("request-1", 0, Operation::LlmComplete(llm()));
    wrong_major.protocol_major = PROTOCOL_MAJOR + 1;
    assert!(matches!(
        validator.validate(&wrong_major),
        Err(ProtocolError::UnsupportedMajor { .. })
    ));
    assert!(matches!(
        validator.validate(&start("other", 0, Operation::LlmComplete(llm()))),
        Err(ProtocolError::WrongRequestId { .. })
    ));
    assert!(matches!(
        validator.validate(&start("request-1", 1, Operation::LlmComplete(llm()))),
        Err(ProtocolError::SequenceGap {
            expected: 0,
            got: 1
        })
    ));
}

#[test]
fn readiness_negotiates_minor_only_with_current_major() {
    let request = ReadinessRequestV1 {
        supported_majors: vec![PROTOCOL_MAJOR],
        supported_minors: vec![0, 3, 2],
        deadline_unix_ms: 1,
        client_instance_id: "client".into(),
        requested_capabilities: vec![],
    };
    assert_eq!(negotiate_minor(&request, &[0, 1, 2]).unwrap(), 2);
    let no_major = ReadinessRequestV1 {
        supported_majors: vec![PROTOCOL_MAJOR + 1],
        ..request.clone()
    };
    assert_eq!(
        negotiate_minor(&no_major, &[0]),
        Err(ReadinessNegotiationError::MajorMismatch {
            server_major: PROTOCOL_MAJOR
        })
    );
    let no_minor = ReadinessRequestV1 {
        supported_minors: vec![4],
        ..request
    };
    assert_eq!(
        negotiate_minor(&no_minor, &[0]),
        Err(ReadinessNegotiationError::MinorMismatch {
            major: PROTOCOL_MAJOR
        })
    );
}
