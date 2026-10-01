//! Observable duplex behavior through the same driver and dialects used by providers.
use super::audio::{AudioSessionCommand as Command, AudioSessionEvent as Event};
use super::audio_driver::AudioSessionDriver;
use super::audio_wire::{AudioSocket, AudioWireDialect, AudioWireEvent};
use super::{gemini_audio::GeminiAudioWire, openai_audio::OpenAiAudioWire};
use crate::error::Result;
use crate::llm_providers::tool_invocation::McpToolCatalog;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};

#[derive(Default)]
struct SocketTrace {
    sent: Vec<Value>,
    closed: bool,
}
struct ScriptedAudioSocket {
    incoming: VecDeque<Value>,
    trace: Arc<Mutex<SocketTrace>>,
}
impl AudioSocket for ScriptedAudioSocket {
    fn send(&mut self, message: Value) -> Result<()> {
        self.trace.lock().unwrap().sent.push(message);
        Ok(())
    }
    fn receive(&mut self) -> Result<Option<Value>> {
        self.incoming.pop_front().map(Some).ok_or_else(|| {
            super::audio::invalid_audio("script exhausted", "client close after final event")
        })
    }
    fn close(&mut self) {
        self.trace.lock().unwrap().closed = true;
    }
}

fn openai() -> OpenAiAudioWire {
    OpenAiAudioWire {
        model: "gpt-realtime".into(),
        voice: "marin".into(),
        instructions: "test".into(),
        tools: vec![],
        response_id: String::new(),
        response_active: false,
    }
}
fn gemini() -> GeminiAudioWire {
    GeminiAudioWire {
        model: "gemini-live".into(),
        voice: "Aoede".into(),
        instructions: "test".into(),
        tools: vec![],
        turn: 0,
        response_active: false,
        input_transcript: String::new(),
        suppress_response: false,
        resumption_handle: None,
        reconnect_requested: false,
    }
}
fn session_events(events: Vec<AudioWireEvent>) -> Vec<Event> {
    events
        .into_iter()
        .filter_map(|event| match event {
            AudioWireEvent::Session(event) => Some(event),
            _ => None,
        })
        .collect()
}

#[test]
fn duplex_connection_keeps_streaming_microphone_and_output_across_two_turns() {
    let incoming = VecDeque::from([
        json!({"type":"session.updated","session":{"id":"live-1"}}),
        json!({"type":"response.created","response":{"id":"r1"}}),
        json!({"type":"response.output_audio.delta","response_id":"r1","item_id":"a1","delta":"AQACAA=="}),
        json!({"type":"response.done","response":{"id":"r1","status":"completed"}}),
        json!({"type":"response.created","response":{"id":"r2"}}),
        json!({"type":"response.output_audio.delta","response_id":"r2","item_id":"a2","delta":"AwAEAA=="}),
        json!({"type":"response.done","response":{"id":"r2","status":"completed"}}),
    ]);
    let trace = Arc::new(Mutex::new(SocketTrace::default()));
    let socket = ScriptedAudioSocket {
        incoming,
        trace: Arc::clone(&trace),
    };
    let (send, receive) = mpsc::channel();
    send.send(Command::Text("opening".into())).unwrap();
    let mut events = Vec::new();
    AudioSessionDriver::new(
        Box::new(socket),
        Box::new(openai()),
        Arc::new(McpToolCatalog::empty("fake")),
    )
    .run(receive, &mut |event| {
        if matches!(event, Event::Audio { .. }) {
            send.send(Command::Pcm(vec![1, 0, 2, 0])).unwrap();
        }
        if matches!(&event, Event::ResponseFinished {response_id} if response_id=="r2") {
            send.send(Command::Close).unwrap();
        }
        events.push(event);
        Ok(())
    })
    .unwrap();
    let trace = trace.lock().unwrap();
    assert!(trace.closed);
    assert_eq!(
        trace
            .sent
            .iter()
            .filter(|event| event["type"] == "session.update")
            .count(),
        1
    );
    assert_eq!(
        trace
            .sent
            .iter()
            .filter(|event| event["type"] == "input_audio_buffer.append")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Audio { .. }))
            .count(),
        2
    );
    assert_eq!(events.last(), Some(&Event::Closed));
}

#[test]
fn microphone_pause_drops_frames_and_resume_restores_input() {
    let incoming = VecDeque::from([
        json!({"type":"session.updated"}),
        json!({"type":"response.created","response":{"id":"last"}}),
    ]);
    let trace = Arc::new(Mutex::new(SocketTrace::default()));
    let (send, receive) = mpsc::channel();
    for command in [
        Command::Pause,
        Command::Pcm(vec![1, 0]),
        Command::Resume,
        Command::Pcm(vec![2, 0]),
    ] {
        send.send(command).unwrap();
    }
    AudioSessionDriver::new(
        Box::new(ScriptedAudioSocket {
            incoming,
            trace: Arc::clone(&trace),
        }),
        Box::new(openai()),
        Arc::new(McpToolCatalog::empty("fake")),
    )
    .run(receive, &mut |event| {
        if matches!(event, Event::ResponseStarted { .. }) {
            send.send(Command::Close).unwrap();
        }
        Ok(())
    })
    .unwrap();
    let trace = trace.lock().unwrap();
    let frames: Vec<_> = trace
        .sent
        .iter()
        .filter(|event| event["type"] == "input_audio_buffer.append")
        .collect();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["audio"], "AgA=");
}

#[test]
fn interrupted_openai_response_drops_late_audio_and_truncates_played_position() {
    let mut wire = openai();
    wire.receive(json!({"type":"response.created","response":{"id":"r1"}}))
        .unwrap();
    let messages = wire
        .command(Command::Interrupt {
            item_id: Some("item1".into()),
            played_ms: 250,
        })
        .unwrap();
    assert_eq!(messages[0]["type"], "response.cancel");
    assert_eq!(messages[1]["audio_end_ms"], 250);
    assert!(
        wire.receive(
            json!({"type":"response.output_audio.delta","response_id":"r1","delta":"AAA="})
        )
        .unwrap()
        .is_empty()
    );
    wire.receive(json!({"type":"response.created","response":{"id":"r2"}}))
        .unwrap();
    assert!(
        wire.receive(json!({"type":"response.done","response":{"id":"r1","status":"cancelled"}}))
            .unwrap()
            .is_empty()
    );
    assert!(wire.response_active);
}

#[test]
fn openai_parallel_tools_resume_generation_only_after_last_result() {
    let mut wire = openai();
    wire.receive(json!({"type":"response.created","response":{"id":"r1"}}))
        .unwrap();
    let output = wire
        .receive(
            json!({"type":"response.done","response":{"id":"r1","status":"completed","output":[
        {"type":"function_call","call_id":"c1","name":"canvas","arguments":"{}"},
        {"type":"function_call","call_id":"c2","name":"knowledge","arguments":"{}"}]}}),
        )
        .unwrap();
    assert_eq!(
        output
            .iter()
            .filter(|event| matches!(event, AudioWireEvent::Tool(_)))
            .count(),
        2
    );
    assert_eq!(wire.tool_result("c1", "canvas", json!({}), false).len(), 1);
    assert_eq!(
        wire.tool_result("c2", "knowledge", json!({}), true)[1]["type"],
        "response.create"
    );
}

#[test]
fn gemini_requests_native_audio_and_emits_audio_before_turn_completion() {
    let mut wire = gemini();
    assert_eq!(
        wire.setup()["setup"]["generationConfig"]["responseModalities"],
        json!(["AUDIO"])
    );
    let events=session_events(wire.receive(json!({"serverContent":{"modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=24000","data":"AQACAA=="}}]}}})).unwrap());
    assert_eq!(
        events,
        vec![
            Event::ResponseStarted {
                response_id: "gemini-audio-1".into()
            },
            Event::Audio {
                response_id: "gemini-audio-1".into(),
                item_id: None,
                pcm: vec![1, 0, 2, 0],
                sample_rate: 24000
            }
        ]
    );
    assert_eq!(
        session_events(
            wire.receive(json!({"serverContent":{"turnComplete":true}}))
                .unwrap()
        ),
        vec![Event::ResponseFinished {
            response_id: "gemini-audio-1".into()
        }]
    );
}

#[test]
fn invalid_pcm_closes_connection_without_sending_microphone_bytes() {
    let trace = Arc::new(Mutex::new(SocketTrace::default()));
    let (send, receive) = mpsc::channel();
    send.send(Command::Pcm(vec![1])).unwrap();
    let socket = ScriptedAudioSocket {
        incoming: VecDeque::from([json!({"type":"session.updated"})]),
        trace: Arc::clone(&trace),
    };
    let error = AudioSessionDriver::new(
        Box::new(socket),
        Box::new(openai()),
        Arc::new(McpToolCatalog::empty("fake")),
    )
    .run(receive, &mut |_| Ok(()))
    .unwrap_err();
    assert!(error.to_string().contains("even PCM16"));
    assert!(trace.lock().unwrap().closed);
}

#[test]
fn gemini_rotation_waits_for_a_fresh_resumable_checkpoint() {
    let mut wire = gemini();
    wire.receive(json!({"sessionResumptionUpdate":{"resumable":true,"newHandle":"old"}}))
        .unwrap();
    wire.receive(json!({"sessionResumptionUpdate":{"resumable":false}}))
        .unwrap();
    assert!(
        wire.receive(json!({"goAway":{"timeLeft":"30s"}}))
            .unwrap()
            .is_empty()
    );
    let events = wire
        .receive(json!({"sessionResumptionUpdate":{"resumable":true,"newHandle":"fresh"}}))
        .unwrap();
    assert!(matches!(events.as_slice(), [AudioWireEvent::Reconnect]));
    assert_eq!(
        wire.setup()["setup"]["sessionResumption"]["handle"],
        "fresh"
    );
}

#[test]
fn paused_driver_never_delivers_late_provider_audio() {
    let trace = Arc::new(Mutex::new(SocketTrace::default()));
    let (send, receive) = mpsc::channel();
    send.send(Command::Pause).unwrap();
    let socket = ScriptedAudioSocket {
        trace,
        incoming: VecDeque::from([
            json!({"type":"session.updated"}),
            json!({"type":"response.created","response":{"id":"r1"}}),
            json!({"type":"response.output_audio.delta","response_id":"r1","delta":"AAA="}),
            json!({"type":"response.done","response":{"id":"r1","status":"completed"}}),
        ]),
    };
    AudioSessionDriver::new(
        Box::new(socket),
        Box::new(openai()),
        Arc::new(McpToolCatalog::empty("fake")),
    )
    .run(receive, &mut |event| {
        assert!(!matches!(event, Event::Audio { .. }));
        if matches!(event, Event::ResponseFinished { .. }) {
            send.send(Command::Close).unwrap();
        }
        Ok(())
    })
    .unwrap();
}
