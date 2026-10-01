use crate::config::EngineConfig;
use crate::error::{NeuralError, Result, require_non_empty};
use crate::process::{
    SpawnedEnvelope, SpawnedEnvelopeKind, SpawnedOperation, SpawnedWorker, StreamControl,
};
use crate::types::EngineMetadata;
use crate::voice2text::{Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_VOICE2TEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub struct SpawnedVoice2TextEngine {
    worker: SpawnedWorker,
}

impl SpawnedVoice2TextEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            worker: SpawnedWorker::new(config.spawn)?,
        })
    }

    pub fn transcribe(&self, request: &Voice2TextRequest) -> Result<Voice2TextResponse> {
        validate_request(request)?;
        let response = self
            .worker
            .run_protobuf(voice2text_request(request, SpawnedOperation::VoiceToText))?;
        validate_response(voice2text_response(response)?)
    }

    pub fn stream(
        &self,
        request: &Voice2TextRequest,
        control: StreamControl,
    ) -> Result<Vec<Voice2TextStreamEvent>> {
        validate_request(request)?;
        let mut events = Vec::new();
        self.worker.run_protobuf_stream(
            voice2text_request(request, SpawnedOperation::VoiceToTextStream),
            control,
            &mut |event| {
                events.push(voice2text_stream_event(event)?);
                Ok(())
            },
        )?;
        Ok(events)
    }
}

fn voice2text_request(request: &Voice2TextRequest, operation: SpawnedOperation) -> SpawnedEnvelope {
    let request_id = NEXT_VOICE2TEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let mut envelope = SpawnedEnvelope::request(operation, format!("voice-text-{request_id}"));
    envelope.binary = request.audio.clone();
    envelope.media_type = request.media_type.clone();
    envelope.model = request.model.clone().unwrap_or_default();
    envelope
}

fn validate_request(request: &Voice2TextRequest) -> Result<()> {
    if request.audio.is_empty() {
        return Err(NeuralError::InvalidValue {
            value: "empty audio".to_string(),
            expected: "non-empty voice audio bytes".to_string(),
        });
    }
    require_non_empty(&request.media_type, "non-empty audio media type")
}

fn validate_response(response: Voice2TextResponse) -> Result<Voice2TextResponse> {
    require_non_empty(&response.transcript, "non-empty transcript")?;
    Ok(response)
}

fn voice2text_response(response: SpawnedEnvelope) -> Result<Voice2TextResponse> {
    Ok(Voice2TextResponse {
        transcript: response.text,
        language: non_empty(response.language),
        confidence: response.has_confidence.then_some(response.confidence),
        segments: response
            .segments_json
            .into_iter()
            .map(decode_json)
            .collect::<Result<Vec<_>>>()?,
        metadata: EngineMetadata {
            engine_id: response.engine_id,
            model: non_empty(response.model),
            metadata: decode_metadata(response.metadata_json)?,
        },
    })
}

fn voice2text_stream_event(event: SpawnedEnvelope) -> Result<Voice2TextStreamEvent> {
    match event.envelope_kind()? {
        SpawnedEnvelopeKind::Data => Ok(Voice2TextStreamEvent::TranscriptChunk {
            sequence: event.sequence,
            text: event.text,
            is_final: event.is_final,
        }),
        SpawnedEnvelopeKind::Complete => Ok(Voice2TextStreamEvent::Complete),
        SpawnedEnvelopeKind::Failure => Ok(Voice2TextStreamEvent::Error {
            message: event.error_message,
        }),
        kind => Err(NeuralError::MalformedPayload {
            value: format!("{kind:?}"),
            expected: "voice2text data, completion, or failure".into(),
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
    decode_json(bytes)
}

fn decode_json(bytes: Vec<u8>) -> Result<Value> {
    serde_json::from_slice(&bytes).map_err(|source| NeuralError::Json {
        value: String::from_utf8_lossy(&bytes).into(),
        expected: "spawned engine JSON metadata value".into(),
        source,
    })
}
