//! Speech-to-text and text-to-speech through the OpenRouter audio HTTP API.
//!
//! Both adapters implement the `speech` seam, so a server can use OpenRouter
//! instead of locally spawned engines.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

use crate::{
    NeuralError, Result,
    llm_providers::contract::{LlmHttpClient, LlmHttpRequest},
    speech::{SpeechRecognizer, SpeechSynthesizer, ensure_active},
    text2voice::{
        Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
    },
    types::EngineMetadata,
    voice2text::{
        Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent, Voice2TextStreamEventSink,
    },
};

// shortcut: OpenRouter PCM is assumed to be 24 kHz mono s16le (true for OpenAI, Gemini and MAI voices), make it configurable when a model with another rate is deployed.
const PCM_SAMPLE_RATE_HZ: u32 = 24_000;

#[derive(Clone, Eq, PartialEq)]
pub struct OpenRouterSpeechConfig {
    pub endpoint: String,
    pub credential: String,
    /// Server-owned model slug; always used. Request models name local engines and are ignored.
    pub model: String,
    /// Server-owned TTS voice; overrides `request.voice_id` when `Some`.
    pub voice: Option<String>,
    pub timeout_ms: u64,
}

// The credential must never reach logs through `{:?}`.
impl fmt::Debug for OpenRouterSpeechConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenRouterSpeechConfig")
            .field("endpoint", &self.endpoint)
            .field("credential", &"<redacted>")
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

pub struct OpenRouterSpeechRecognizer {
    config: OpenRouterSpeechConfig,
    http_client: Arc<dyn LlmHttpClient>,
}

impl OpenRouterSpeechRecognizer {
    pub fn new(config: OpenRouterSpeechConfig, http_client: Arc<dyn LlmHttpClient>) -> Self {
        Self {
            config,
            http_client,
        }
    }
}

pub struct OpenRouterSpeechSynthesizer {
    config: OpenRouterSpeechConfig,
    http_client: Arc<dyn LlmHttpClient>,
}

impl OpenRouterSpeechSynthesizer {
    pub fn new(config: OpenRouterSpeechConfig, http_client: Arc<dyn LlmHttpClient>) -> Self {
        Self {
            config,
            http_client,
        }
    }

    /// Returns the model used and the raw 24 kHz mono s16le PCM bytes.
    fn pcm(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
    ) -> Result<(String, Vec<u8>)> {
        let model = self.config.model.clone();
        let mut payload = json!({
            "model": model,
            "input": request.text,
            "response_format": "pcm",
        });
        if let Some(voice) = self.config.voice.as_ref().or(request.voice_id.as_ref()) {
            payload["voice"] = json!(voice);
        }
        let audio = self.http_client.post_bytes(&http_request(
            &self.config,
            control,
            "audio/speech",
            payload,
        ))?;
        if audio.is_empty() {
            return Err(NeuralError::ProviderFailed {
                provider_id: "openrouter-tts".into(),
                message: "speech response body is empty".into(),
            });
        }
        Ok((model, audio))
    }
}

impl SpeechRecognizer for OpenRouterSpeechRecognizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }

    fn transcribe(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
    ) -> Result<Voice2TextResponse> {
        ensure_active(control, "openrouter-stt")?;
        let format = audio_format(&request.media_type)?;
        let model = self.config.model.clone();
        let payload = json!({
            "model": model,
            "input_audio": {
                "data": base64::engine::general_purpose::STANDARD.encode(&request.audio),
                "format": format,
            },
        });
        let response = self.http_client.post_json(&http_request(
            &self.config,
            control,
            "audio/transcriptions",
            payload,
        ))?;
        ensure_active(control, "openrouter-stt")?;
        let transcript = response
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| NeuralError::ProviderFailed {
                provider_id: "openrouter-stt".into(),
                message: "transcription response has no string `text`".into(),
            })?;
        Ok(Voice2TextResponse {
            transcript: transcript.to_string(),
            language: None,
            confidence: None,
            segments: vec![],
            metadata: EngineMetadata {
                engine_id: "openrouter-stt".into(),
                model: Some(model),
                metadata: json!({ "usage": response.get("usage").cloned().unwrap_or(Value::Null) }),
            },
        })
    }

    fn stream_with_events(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
        on_event: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()> {
        let response = SpeechRecognizer::transcribe(self, request, control)?;
        on_event(Voice2TextStreamEvent::TranscriptChunk {
            sequence: 0,
            text: response.transcript,
            is_final: true,
        })?;
        on_event(Voice2TextStreamEvent::Complete)
    }
}

impl SpeechSynthesizer for OpenRouterSpeechSynthesizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }

    fn synthesize(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
    ) -> Result<Text2VoiceResponse> {
        ensure_active(control, "openrouter-tts")?;
        let (model, pcm) = self.pcm(request, control)?;
        ensure_active(control, "openrouter-tts")?;
        Ok(Text2VoiceResponse {
            audio: wav_from_pcm(&pcm),
            media_type: "audio/wav".into(),
            sample_rate_hz: Some(PCM_SAMPLE_RATE_HZ),
            metadata: EngineMetadata {
                engine_id: "openrouter-tts".into(),
                model: Some(model),
                metadata: Value::Null,
            },
        })
    }

    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        ensure_active(control, "openrouter-tts")?;
        // shortcut: the whole utterance is buffered before the first chunk, switch to incremental body reads when first-audio latency matters.
        let (_, pcm) = self.pcm(request, control)?;
        ensure_active(control, "openrouter-tts")?;
        on_event(Text2VoiceStreamEvent::AudioChunk {
            sequence: 0,
            audio: pcm,
            media_type: "audio/pcm;rate=24000;format=s16le".into(),
        })?;
        on_event(Text2VoiceStreamEvent::Complete)
    }
}

/// Builds one call with the invocation's remaining time as its ceiling.
fn http_request(
    config: &OpenRouterSpeechConfig,
    control: &InvocationControl,
    path: &str,
    payload: Value,
) -> LlmHttpRequest {
    LlmHttpRequest {
        timeout: Some(Duration::from_millis(config.timeout_ms).min(control.remaining())),
        endpoint: format!("{}/{path}", config.endpoint.trim_end_matches('/')),
        credential: config.credential.clone(),
        headers: BTreeMap::new(),
        payload,
    }
}

/// Maps a media type (parameters ignored) to the OpenRouter `format` value.
fn audio_format(media_type: &str) -> Result<&'static str> {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Ok(match essence.as_str() {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" => "flac",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        "audio/aac" => "aac",
        _ => {
            return Err(NeuralError::InvalidValue {
                value: media_type.to_string(),
                expected: "OpenRouter STT audio media type (wav, mp3, flac, m4a, ogg, webm, aac)"
                    .to_string(),
            });
        }
    })
}

fn wav_from_pcm(pcm: &[u8]) -> Vec<u8> {
    let data_size = pcm.len() as u32;
    let mut bytes = Vec::with_capacity(44 + pcm.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&PCM_SAMPLE_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&(PCM_SAMPLE_RATE_HZ * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    bytes.extend_from_slice(pcm);
    bytes
}
