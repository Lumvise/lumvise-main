use crate::config::EngineConfig;
use crate::error::{NeuralError, Result, require_non_empty};
use crate::process::{SpawnedEnvelope, SpawnedOperation, SpawnedWorker};
use crate::text2vector::{Text2VectorRequest, Text2VectorResponse};
use crate::types::EngineMetadata;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_VECTOR_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub struct SpawnedText2VectorEngine {
    expected_dimensions: Option<usize>,
    worker: SpawnedWorker,
}

impl SpawnedText2VectorEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            expected_dimensions: config.expected_dimensions,
            worker: SpawnedWorker::new(config.spawn)?,
        })
    }

    pub fn embed(&self, request: &Text2VectorRequest) -> Result<Text2VectorResponse> {
        require_non_empty(&request.text, "non-empty text to vectorize")?;
        let response = self.worker.run_protobuf(vector_request(request))?;
        let response = vector_response(response)?;
        self.validate_response(response)
    }

    fn validate_response(&self, response: Text2VectorResponse) -> Result<Text2VectorResponse> {
        if response.vector.len() != response.dimensions {
            return Err(NeuralError::InvalidValue {
                value: response.vector.len().to_string(),
                expected: format!("vector length matching dimensions {}", response.dimensions),
            });
        }
        if let Some(expected) = self.expected_dimensions
            && response.dimensions != expected
        {
            return Err(NeuralError::InvalidValue {
                value: response.dimensions.to_string(),
                expected: format!("vector dimensions {expected}"),
            });
        }
        Ok(response)
    }
}

fn vector_request(request: &Text2VectorRequest) -> SpawnedEnvelope {
    let request_id = NEXT_VECTOR_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let mut envelope = SpawnedEnvelope::request(
        SpawnedOperation::TextVector,
        format!("text-vector-{request_id}"),
    );
    envelope.text = request.text.clone();
    envelope.model = request.model.clone().unwrap_or_default();
    envelope
}

fn vector_response(response: SpawnedEnvelope) -> Result<Text2VectorResponse> {
    Ok(Text2VectorResponse {
        vector: response.vector,
        dimensions: usize::try_from(response.dimensions).map_err(|error| {
            NeuralError::MalformedPayload {
                value: error.to_string(),
                expected: "vector dimensions representable by this host".into(),
            }
        })?,
        normalized: response.normalized,
        metadata: EngineMetadata {
            engine_id: response.engine_id,
            model: non_empty(response.model),
            metadata: decode_metadata(response.metadata_json)?,
        },
    })
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
