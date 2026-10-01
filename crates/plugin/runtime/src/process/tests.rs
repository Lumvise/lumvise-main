use super::*;
use lumvise_plugin_protocol::ProtocolVersion;
use std::io::Cursor;

fn invoke_message(invocation_id: &str) -> WireMessage {
    WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::HostInvoke {
            session_id: "session".into(),
            invocation_id: invocation_id.into(),
            capability_id: "capability".into(),
            input: Value::Null,
        },
    }
}

#[test]
fn frame_encoding_precedes_poisoned_writer_lock() {
    let writer = Mutex::new(Vec::<u8>::new());
    let _ = std::panic::catch_unwind(|| {
        let _guard = writer.lock().expect("writer lock");
        panic!("poison writer");
    });
    let invalid = WireMessage {
        protocol: ProtocolVersion::new(0, 0),
        body: MessageBody::HostShutdown {
            session_id: "session".into(),
            reason: None,
        },
    };
    assert!(matches!(
        encode_then_write(&writer, &invalid),
        Err(FrameWriteError::Encode(_))
    ));
}

#[test]
fn concurrent_encoded_frames_remain_decodable() {
    let writer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let handles = (0..16)
        .map(|index| {
            let writer = Arc::clone(&writer);
            std::thread::spawn(move || {
                encode_then_write(writer.as_ref(), &invoke_message(&format!("invoke-{index}")))
                    .expect("write encoded frame");
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().expect("frame writer thread");
    }
    let bytes = writer.lock().expect("writer bytes").clone();
    let mut reader = Cursor::new(bytes);
    let mut ids = Vec::new();
    for _ in 0..16 {
        let message = FrameCodec::default()
            .read_from(&mut reader)
            .expect("decode concurrent frame");
        let MessageBody::HostInvoke { invocation_id, .. } = message.body else {
            panic!("expected invoke frame");
        };
        ids.push(invocation_id);
    }
    ids.sort();
    let mut expected = (0..16)
        .map(|index| format!("invoke-{index}"))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(ids, expected);
}
