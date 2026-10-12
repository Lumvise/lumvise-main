use std::sync::{Arc, Mutex};

use base64::Engine;
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::text2voice::{Text2VoiceRequest, Text2VoiceStreamEvent};
use lumvise_neural_core::voice2text::{Voice2TextRequest, Voice2TextStreamEvent};
use lumvise_neural_core::{
    NeuralError, OpenRouterSpeechConfig, OpenRouterSpeechRecognizer, OpenRouterSpeechSynthesizer,
    Result, SpeechRecognizer, SpeechSynthesizer,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

/// Fake transport: records every call and answers with canned JSON or bytes.
struct FakeHttpClient {
    calls: Mutex<Vec<LlmHttpRequest>>,
    json: Value,
    bytes: Vec<u8>,
}

impl FakeHttpClient {
    fn new(json: Value, bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            json,
            bytes,
        })
    }

    fn calls(&self) -> Vec<LlmHttpRequest> {
        self.calls.lock().unwrap().clone()
    }
}

impl LlmHttpClient for FakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(self.json.clone())
    }

    fn post_bytes(&self, request: &LlmHttpRequest) -> Result<Vec<u8>> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(self.bytes.clone())
    }

    fn stream_text(&self, _request: &LlmHttpRequest) -> Result<Vec<String>> {
        unreachable!("speech adapters never stream text")
    }
}

fn config() -> OpenRouterSpeechConfig {
    OpenRouterSpeechConfig {
        endpoint: "https://openrouter.test/api/v1/".into(),
        credential: "test-key".into(),
        model: "default-model".into(),
        voice: Some("alloy".into()),
        timeout_ms: 30_000,
    }
}

fn stt_request(media_type: &str, model: Option<&str>) -> Voice2TextRequest {
    Voice2TextRequest {
        audio: b"fake-audio".to_vec(),
        media_type: media_type.into(),
        model: model.map(Into::into),
    }
}

#[test]
fn stt_sends_base64_audio_and_returns_transcript() {
    let fake = FakeHttpClient::new(
        json!({"text": "hello there", "usage": {"seconds": 1.5}}),
        Vec::new(),
    );
    let recognizer = OpenRouterSpeechRecognizer::new(config(), fake.clone());

    let response = recognizer
        .transcribe(
            &stt_request("audio/wav", Some("request-model")),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].endpoint,
        "https://openrouter.test/api/v1/audio/transcriptions"
    );
    assert_eq!(calls[0].credential, "test-key");
    assert_eq!(calls[0].payload["model"], "default-model");
    assert_eq!(calls[0].payload["input_audio"]["format"], "wav");
    let data = calls[0].payload["input_audio"]["data"].as_str().unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap(),
        b"fake-audio"
    );
    assert_eq!(response.transcript, "hello there");
    assert_eq!(response.metadata.engine_id, "openrouter-stt");
    assert_eq!(response.metadata.model.as_deref(), Some("default-model"));
    assert_eq!(response.metadata.metadata["usage"]["seconds"], 1.5);
}

#[test]
fn stt_uses_config_model_when_request_has_none() {
    let fake = FakeHttpClient::new(json!({"text": "hi"}), Vec::new());
    let recognizer = OpenRouterSpeechRecognizer::new(config(), fake.clone());

    recognizer
        .transcribe(
            &stt_request("audio/mpeg; codecs=mp3", None),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls[0].payload["model"], "default-model");
    assert_eq!(calls[0].payload["input_audio"]["format"], "mp3");
}

#[test]
fn stt_rejects_unsupported_media_type_without_http_call() {
    let fake = FakeHttpClient::new(json!({"text": "unused"}), Vec::new());
    let recognizer = OpenRouterSpeechRecognizer::new(config(), fake.clone());

    let error = recognizer
        .transcribe(
            &stt_request("audio/amr", None),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();

    assert!(
        matches!(&error, NeuralError::InvalidValue { value, .. } if value == "audio/amr"),
        "unexpected error: {error:?}"
    );
    assert!(fake.calls().is_empty());
}

#[test]
fn tts_sends_pcm_request_and_synthesizes_wav() {
    let pcm = vec![1_u8, 0, 2, 0, 3, 0];
    let fake = FakeHttpClient::new(Value::Null, pcm.clone());
    let synthesizer = OpenRouterSpeechSynthesizer::new(config(), fake.clone());

    let response = synthesizer
        .synthesize(
            &Text2VoiceRequest {
                text: "say this".into(),
                voice_id: Some("nova".into()),
                model: None,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();

    let calls = fake.calls();
    assert_eq!(
        calls[0].endpoint,
        "https://openrouter.test/api/v1/audio/speech"
    );
    assert_eq!(calls[0].payload["input"], "say this");
    assert_eq!(calls[0].payload["voice"], "alloy");
    assert_eq!(calls[0].payload["response_format"], "pcm");
    assert_eq!(calls[0].payload["model"], "default-model");
    assert_eq!(response.media_type, "audio/wav");
    assert_eq!(response.sample_rate_hz, Some(24_000));
    assert_eq!(&response.audio[0..4], b"RIFF");
    assert_eq!(&response.audio[44..], pcm.as_slice());
}

#[test]
fn request_model_and_voice_do_not_override_configured_model_and_voice() {
    let fake = FakeHttpClient::new(json!({"text": "unused"}), vec![1, 0]);
    let recognizer = OpenRouterSpeechRecognizer::new(config(), fake.clone());
    let synthesizer = OpenRouterSpeechSynthesizer::new(config(), fake.clone());

    recognizer
        .transcribe(
            &stt_request("audio/wav", Some("whisper-local")),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    synthesizer
        .synthesize(
            &Text2VoiceRequest {
                text: "local voice".into(),
                voice_id: Some("af_heart".into()),
                model: Some("kokoro-local".into()),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();

    let calls = fake.calls();
    assert_eq!(calls[0].payload["model"], "default-model");
    assert_eq!(calls[1].payload["model"], "default-model");
    assert_eq!(calls[1].payload["voice"], "alloy");
}

#[test]
fn tts_uses_request_voice_when_config_has_none() {
    let fake = FakeHttpClient::new(Value::Null, vec![1, 0]);
    let mut config = config();
    config.voice = None;
    let synthesizer = OpenRouterSpeechSynthesizer::new(config, fake.clone());

    synthesizer
        .synthesize(
            &Text2VoiceRequest {
                text: "request voice".into(),
                voice_id: Some("nova".into()),
                model: None,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();

    assert_eq!(fake.calls()[0].payload["voice"], "nova");
}

#[test]
fn tts_omits_voice_when_request_and_config_have_none() {
    let fake = FakeHttpClient::new(Value::Null, vec![1, 0]);
    let mut config = config();
    config.voice = None;
    let synthesizer = OpenRouterSpeechSynthesizer::new(config, fake.clone());

    synthesizer
        .synthesize(
            &Text2VoiceRequest {
                text: "no voice".into(),
                voice_id: None,
                model: None,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();

    assert!(fake.calls()[0].payload.get("voice").is_none());
}

#[test]
fn tts_stream_emits_one_pcm_chunk_then_complete() {
    let pcm = vec![7_u8, 0, 8, 0];
    let fake = FakeHttpClient::new(Value::Null, pcm.clone());
    let synthesizer = OpenRouterSpeechSynthesizer::new(config(), fake);
    let mut events = Vec::new();

    synthesizer
        .stream_with_events(
            &Text2VoiceRequest {
                text: "stream".into(),
                voice_id: None,
                model: None,
            },
            &InvocationControl::sixty_seconds(),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            Text2VoiceStreamEvent::AudioChunk {
                sequence: 0,
                audio: pcm,
                media_type: "audio/pcm;rate=24000;format=s16le".into(),
            },
            Text2VoiceStreamEvent::Complete,
        ]
    );
}

#[test]
fn stt_stream_emits_final_transcript_then_complete() {
    let fake = FakeHttpClient::new(json!({"text": "streamed"}), Vec::new());
    let recognizer = OpenRouterSpeechRecognizer::new(config(), fake);
    let mut events = Vec::new();

    recognizer
        .stream_with_events(
            &stt_request("audio/wav", None),
            &InvocationControl::sixty_seconds(),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(
        events,
        vec![
            Voice2TextStreamEvent::TranscriptChunk {
                sequence: 0,
                text: "streamed".into(),
                is_final: true,
            },
            Voice2TextStreamEvent::Complete,
        ]
    );
}

#[test]
fn cancelled_invocation_fails_before_any_http_call() {
    let fake = FakeHttpClient::new(json!({"text": "unused"}), vec![1, 0]);
    let control = InvocationControl::sixty_seconds();
    control.cancel();

    let stt = OpenRouterSpeechRecognizer::new(config(), fake.clone())
        .transcribe(&stt_request("audio/wav", None), &control);
    let tts = OpenRouterSpeechSynthesizer::new(config(), fake.clone()).synthesize(
        &Text2VoiceRequest {
            text: "cancelled".into(),
            voice_id: None,
            model: None,
        },
        &control,
    );

    assert!(stt.is_err());
    assert!(tts.is_err());
    assert!(fake.calls().is_empty());
}

#[test]
fn config_debug_redacts_credential() {
    let debug = format!("{:?}", config());
    assert!(
        !debug.contains("test-key"),
        "debug leaked credential: {debug}"
    );
    assert!(debug.contains("<redacted>"), "debug: {debug}");
}
