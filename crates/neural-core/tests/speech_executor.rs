use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use lumvise_neural_core::text2voice::{
    Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
};
use lumvise_neural_core::types::EngineMetadata;
use lumvise_neural_core::voice2text::{
    Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEventSink,
};
use lumvise_neural_core::{
    Result, SpeechExecutionError, SpeechRecognizer, SpeechSynthesizer, SpeechToTextExecutor,
    TextToSpeechExecutor, chunk_for_speech, sanitize_for_speech,
};
use lumvise_resource_routing::InvocationControl;
use parking_lot::Mutex;

struct TestRecognizer;

impl SpeechRecognizer for TestRecognizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }

    fn transcribe(
        &self,
        _request: &Voice2TextRequest,
        _control: &InvocationControl,
    ) -> Result<Voice2TextResponse> {
        Ok(Voice2TextResponse {
            transcript: "recognized".to_string(),
            language: Some("en".to_string()),
            confidence: Some(1.0),
            segments: Vec::new(),
            metadata: EngineMetadata {
                engine_id: "test".to_string(),
                model: None,
                metadata: serde_json::Value::Null,
            },
        })
    }

    fn stream_with_events(
        &self,
        _request: &Voice2TextRequest,
        _control: &InvocationControl,
        _on_event: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

fn request() -> Voice2TextRequest {
    Voice2TextRequest {
        audio: vec![0, 1, 2],
        media_type: "audio/wav".to_string(),
        model: None,
    }
}

#[test]
fn speech_executor_propagates_one_invocation_control() {
    let executor = SpeechToTextExecutor::start(Arc::new(TestRecognizer));
    let control = InvocationControl::sixty_seconds();

    assert_eq!(
        executor
            .transcribe(request(), control.clone())
            .unwrap()
            .transcript,
        "recognized"
    );

    control.cancel();
    assert!(matches!(
        executor.transcribe(request(), control),
        Err(SpeechExecutionError::Cancelled)
    ));
}

struct RecordingSynthesizer {
    requests: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

impl SpeechSynthesizer for RecordingSynthesizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }

    fn synthesize(
        &self,
        _request: &Text2VoiceRequest,
        _control: &InvocationControl,
    ) -> Result<Text2VoiceResponse> {
        Ok(Text2VoiceResponse {
            audio: Vec::new(),
            media_type: "audio/pcm;rate=24000;format=s16le".into(),
            sample_rate_hz: Some(24_000),
            metadata: EngineMetadata {
                engine_id: "recording-synthesizer".into(),
                model: None,
                metadata: serde_json::Value::Null,
            },
        })
    }

    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        _control: &InvocationControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        self.requests.lock().push(request.text.clone());
        if self.fail {
            return on_event(Text2VoiceStreamEvent::Error {
                message: "synthetic stream failure".into(),
            });
        }
        on_event(Text2VoiceStreamEvent::AudioChunk {
            sequence: 99,
            audio: vec![request.text.len() as u8],
            media_type: "audio/pcm;rate=24000;format=s16le".into(),
        })?;
        on_event(Text2VoiceStreamEvent::Complete)
    }
}

fn synthesis_request(text: &str) -> Text2VoiceRequest {
    Text2VoiceRequest {
        text: text.into(),
        voice_id: Some("test-voice".into()),
        model: None,
    }
}

fn collect_stream(executor: &TextToSpeechExecutor, text: &str) -> Vec<Text2VoiceStreamEvent> {
    let (sender, receiver) = mpsc::channel();
    executor
        .enqueue_stream(
            synthesis_request(text),
            InvocationControl::sixty_seconds(),
            move |event| {
                let _ = sender.send(event);
                Ok(())
            },
        )
        .unwrap();
    let mut events = Vec::new();
    loop {
        let event = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("stream must emit a terminal event");
        let terminal = matches!(
            event,
            Text2VoiceStreamEvent::Complete | Text2VoiceStreamEvent::Error { .. }
        );
        events.push(event);
        if terminal {
            return events;
        }
    }
}

#[test]
fn streamed_speech_sanitizes_chunks_and_normalizes_sequences() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = TextToSpeechExecutor::start(Arc::new(RecordingSynthesizer {
        requests: Arc::clone(&requests),
        fail: false,
    }));
    let text = "**First sentence has enough detail to clear the minimum.** \
        Second sentence also has enough detail to stream independently. Tail.";
    let expected_chunks = chunk_for_speech(&sanitize_for_speech(text));

    let events = collect_stream(&executor, text);

    assert_eq!(*requests.lock(), expected_chunks);
    assert_eq!(events.len(), expected_chunks.len() + 1);
    for (sequence, event) in events[..expected_chunks.len()].iter().enumerate() {
        assert!(matches!(
            event,
            Text2VoiceStreamEvent::AudioChunk {
                sequence: actual,
                ..
            } if *actual == sequence as u64
        ));
    }
    assert_eq!(events.last(), Some(&Text2VoiceStreamEvent::Complete));
}

#[test]
fn streamed_speech_error_is_terminal_without_complete() {
    let executor = TextToSpeechExecutor::start(Arc::new(RecordingSynthesizer {
        requests: Arc::new(Mutex::new(Vec::new())),
        fail: true,
    }));

    assert_eq!(
        collect_stream(
            &executor,
            "A sentence long enough to reach the speech engine."
        ),
        vec![Text2VoiceStreamEvent::Error {
            message: "synthetic stream failure".into(),
        }]
    );
}
