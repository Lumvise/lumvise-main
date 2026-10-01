use crate::config::EngineConfig;
use crate::error::{NeuralError, Result, require_non_empty};
use crate::process::{
    SpawnedEnvelope, SpawnedEnvelopeKind, SpawnedOperation, SpawnedWorker, StreamControl,
};
use crate::text2voice::speech_text::prepare_text2voice_request;
use crate::text2voice::{
    Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
};
use crate::types::EngineMetadata;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEXT2VOICE_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub struct SpawnedText2VoiceEngine {
    config: EngineConfig,
    worker: SpawnedWorker,
}

impl SpawnedText2VoiceEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        config.validate()?;
        let worker = SpawnedWorker::new(config.spawn.clone())?;
        Ok(Self { config, worker })
    }

    pub fn synthesize(&self, request: &Text2VoiceRequest) -> Result<Text2VoiceResponse> {
        let prepared = prepare_text2voice_request(request);
        require_non_empty(&prepared.text, "non-empty text to synthesize")?;
        let response = self
            .worker
            .run_protobuf(text2voice_request(&prepared, SpawnedOperation::TextToVoice))?;
        validate_audio_response(text2voice_response(response)?)
    }

    pub fn stream(
        &self,
        request: &Text2VoiceRequest,
        control: StreamControl,
    ) -> Result<Vec<Text2VoiceStreamEvent>> {
        let prepared = prepare_text2voice_request(request);
        require_non_empty(&prepared.text, "non-empty text to synthesize")?;
        let mut events = Vec::new();
        self.worker.run_protobuf_stream(
            text2voice_request(&prepared, SpawnedOperation::TextToVoiceStream),
            control,
            &mut |event| {
                events.push(text2voice_stream_event(event)?);
                Ok(())
            },
        )?;
        Ok(events)
    }

    pub fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: StreamControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        let prepared = prepare_text2voice_request(request);
        require_non_empty(&prepared.text, "non-empty text to synthesize")?;
        self.worker.run_protobuf_stream(
            text2voice_request(&prepared, SpawnedOperation::TextToVoiceStream),
            control,
            &mut |event| on_event(text2voice_stream_event(event)?),
        )
    }

    pub fn engine_id(&self) -> &str {
        &self.config.engine_id
    }
}

fn validate_audio_response(response: Text2VoiceResponse) -> Result<Text2VoiceResponse> {
    if response.audio.is_empty() {
        return Err(NeuralError::MalformedPayload {
            value: "empty audio".to_string(),
            expected: "non-empty generated audio bytes".to_string(),
        });
    }
    require_non_empty(&response.media_type, "non-empty audio media type")?;
    Ok(response)
}

fn text2voice_request(request: &Text2VoiceRequest, operation: SpawnedOperation) -> SpawnedEnvelope {
    let request_id = NEXT_TEXT2VOICE_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let mut envelope = SpawnedEnvelope::request(operation, format!("text-voice-{request_id}"));
    envelope.text = request.text.clone();
    envelope.model = request.model.clone().unwrap_or_default();
    envelope.voice_id = request.voice_id.clone().unwrap_or_default();
    envelope
}

fn text2voice_response(response: SpawnedEnvelope) -> Result<Text2VoiceResponse> {
    Ok(Text2VoiceResponse {
        audio: response.binary,
        media_type: response.media_type,
        sample_rate_hz: (response.sample_rate_hz != 0).then_some(response.sample_rate_hz),
        metadata: EngineMetadata {
            engine_id: response.engine_id,
            model: non_empty(response.model),
            metadata: decode_metadata(response.metadata_json)?,
        },
    })
}

fn text2voice_stream_event(event: SpawnedEnvelope) -> Result<Text2VoiceStreamEvent> {
    match event.envelope_kind()? {
        SpawnedEnvelopeKind::Data => Ok(Text2VoiceStreamEvent::AudioChunk {
            sequence: event.sequence,
            audio: event.binary,
            media_type: event.media_type,
        }),
        SpawnedEnvelopeKind::Complete => Ok(Text2VoiceStreamEvent::Complete),
        SpawnedEnvelopeKind::Failure => Ok(Text2VoiceStreamEvent::Error {
            message: event.error_message,
        }),
        kind => Err(NeuralError::MalformedPayload {
            value: format!("{kind:?}"),
            expected: "text2voice data, completion, or failure".into(),
        }),
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn decode_metadata(bytes: Vec<u8>) -> Result<Value> {
    if bytes.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&bytes).map_err(|source| NeuralError::Json {
        value: String::from_utf8_lossy(&bytes).into(),
        expected: "spawned engine metadata JSON".into(),
        source,
    })
}
