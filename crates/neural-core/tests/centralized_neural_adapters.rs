use parking_lot::Mutex;
use std::sync::Arc;

use lumvise_neural_core::{
    CentralizedSpeechRecognizer, CentralizedSpeechSynthesizer, NeuralError, Result,
    SpeechRecognizer, SpeechSynthesizer,
    text2voice::{
        Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
    },
    voice2text::{
        Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent, Voice2TextStreamEventSink,
    },
};
use lumvise_resource_routing::{
    InvocationControl, ResourceInvocationClient, TransportError,
    protocol::{
        InvocationEnvelopeV1, InvocationTerminalStatusV1, InvocationTerminalV1, ReadinessRequestV1,
        ReadinessResponseV1, SpeechToTextResultV1, TextToSpeechResultV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};

struct FakeRecognizer;

impl SpeechRecognizer for FakeRecognizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }
    fn transcribe(
        &self,
        _: &Voice2TextRequest,
        control: &InvocationControl,
    ) -> Result<Voice2TextResponse> {
        if control.is_cancelled() {
            return Err(NeuralError::ProviderFailed {
                provider_id: "fake".into(),
                message: "cancelled".into(),
            });
        }
        Ok(Voice2TextResponse {
            transcript: "spoken".into(),
            language: Some("en".into()),
            confidence: Some(1.0),
            segments: vec![],
            metadata: metadata(),
        })
    }
    fn stream_with_events(
        &self,
        _: &Voice2TextRequest,
        _: &InvocationControl,
        sink: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()> {
        sink(Voice2TextStreamEvent::TranscriptChunk {
            sequence: 0,
            text: "spoken".into(),
            is_final: true,
        })?;
        sink(Voice2TextStreamEvent::Complete)
    }
}

struct FakeSynthesizer;

impl SpeechSynthesizer for FakeSynthesizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }
    fn synthesize(
        &self,
        _: &Text2VoiceRequest,
        _: &InvocationControl,
    ) -> Result<Text2VoiceResponse> {
        Ok(Text2VoiceResponse {
            audio: vec![1, 2],
            media_type: "audio/wav".into(),
            sample_rate_hz: Some(16_000),
            metadata: metadata(),
        })
    }
    fn stream_with_events(
        &self,
        _: &Text2VoiceRequest,
        _: &InvocationControl,
        sink: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        sink(Text2VoiceStreamEvent::AudioChunk {
            sequence: 0,
            audio: vec![1, 2],
            media_type: "audio/wav".into(),
        })?;
        sink(Text2VoiceStreamEvent::Complete)
    }
}

#[test]
fn speech_interfaces_preserve_binary_responses_and_ordered_stream_events() {
    let control = InvocationControl::sixty_seconds();
    let recognizer = FakeRecognizer;
    let synthesizer = FakeSynthesizer;
    recognizer.warmup().unwrap();
    synthesizer.warmup().unwrap();
    let mut transcript_events = Vec::new();
    recognizer
        .stream_with_events(
            &Voice2TextRequest {
                audio: vec![0],
                media_type: "audio/wav".into(),
                model: None,
            },
            &control,
            &mut |event| {
                transcript_events.push(event);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(transcript_events.len(), 2);
    let mut audio_events = Vec::new();
    synthesizer
        .stream_with_events(
            &Text2VoiceRequest {
                text: "hello".into(),
                voice_id: None,
                model: None,
            },
            &control,
            &mut |event| {
                audio_events.push(event);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(audio_events.len(), 2);
}

fn metadata() -> lumvise_neural_core::types::EngineMetadata {
    lumvise_neural_core::types::EngineMetadata {
        engine_id: "fake".into(),
        model: None,
        metadata: serde_json::Value::Null,
    }
}

struct CentralClient {
    starts: Mutex<Vec<InvocationEnvelopeV1>>,
}

impl ResourceInvocationClient for CentralClient {
    fn readiness(
        &self,
        _: &ReadinessRequestV1,
        _: &InvocationControl,
    ) -> std::result::Result<ReadinessResponseV1, TransportError> {
        unreachable!("speech execution does not issue readiness")
    }

    fn invoke(
        &self,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> std::result::Result<Vec<InvocationEnvelopeV1>, TransportError> {
        assert!(!control.is_cancelled());
        assert!(!control.is_expired());
        assert_eq!(envelopes.len(), 1);
        let start = envelopes[0].clone();
        let operation = match start.payload.as_ref() {
            Some(Payload::Start(start)) => start.operation.as_ref().unwrap(),
            _ => panic!("central adapter must send one start envelope"),
        };
        self.starts.lock().push(start.clone());
        let result = match operation {
            Operation::SpeechToText(request) => {
                assert_eq!(request.audio, vec![0, 255, 1]);
                assert_eq!(request.media_type, "audio/wav");
                TerminalResult::SpeechToText(SpeechToTextResultV1 {
                    transcript: "binary transcript".into(),
                    language: Some("en".into()),
                    confidence: Some(0.75),
                    segments: vec![],
                })
            }
            Operation::TextToSpeech(request) => {
                assert_eq!(request.text, "speak");
                assert_eq!(request.voice, "voice");
                TerminalResult::TextToSpeech(TextToSpeechResultV1 {
                    audio: vec![0, 128, 255],
                    media_type: "audio/pcm".into(),
                    sample_rate: Some(24_000),
                    metadata_json: None,
                })
            }
            _ => panic!("central adapter sent unexpected operation"),
        };
        Ok(vec![InvocationEnvelopeV1 {
            protocol_major: start.protocol_major,
            protocol_minor: start.protocol_minor,
            request_id: start.request_id,
            sequence: 0,
            payload: Some(Payload::Terminal(InvocationTerminalV1 {
                status: InvocationTerminalStatusV1::Completed as i32,
                retryable: false,
                outcome_unknown: false,
                error_code: None,
                message: None,
                result: Some(result),
            })),
        }])
    }

    fn cancel(
        &self,
        _: &InvocationEnvelopeV1,
        _: &InvocationControl,
    ) -> std::result::Result<InvocationTerminalV1, TransportError> {
        unreachable!("speech execution does not cancel in this test")
    }
}

#[test]
fn centralized_speech_adapters_use_one_control_and_keep_audio_binary() {
    let backing = Arc::new(CentralClient {
        starts: Mutex::new(Vec::new()),
    });
    let client: Arc<dyn ResourceInvocationClient> = backing.clone();
    let recognizer = CentralizedSpeechRecognizer::new(client.clone(), "desktop-1");
    let synthesizer = CentralizedSpeechSynthesizer::new(client, "desktop-1");
    let control = InvocationControl::sixty_seconds();

    let transcription = recognizer
        .transcribe(
            &Voice2TextRequest {
                audio: vec![0, 255, 1],
                media_type: "audio/wav".into(),
                model: Some("stt".into()),
            },
            &control,
        )
        .unwrap();
    assert_eq!(transcription.transcript, "binary transcript");

    let speech = synthesizer
        .synthesize(
            &Text2VoiceRequest {
                text: "speak".into(),
                voice_id: Some("voice".into()),
                model: Some("tts".into()),
            },
            &control,
        )
        .unwrap();
    assert_eq!(speech.audio, vec![0, 128, 255]);
    assert_eq!(speech.sample_rate_hz, Some(24_000));

    let starts = backing.starts.lock();
    assert_eq!(starts.len(), 2);
    for start in starts.iter() {
        let Payload::Start(start) = start.payload.as_ref().unwrap() else {
            unreachable!()
        };
        assert_eq!(start.deadline_unix_ms, control.deadline_unix_ms());
        assert_eq!(start.client_instance_id, "desktop-1");
    }
}
