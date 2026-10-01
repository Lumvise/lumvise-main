use crate::error::{NeuralError, Result};
use prost::{Enumeration, Message};

pub const SPAWNED_ENGINE_PROTOCOL_MAJOR: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enumeration)]
#[repr(i32)]
pub enum SpawnedOperation {
    Unspecified = 0,
    LlmComplete = 1,
    LlmStream = 2,
    TextVector = 3,
    TextToVoice = 4,
    TextToVoiceStream = 5,
    VoiceToText = 6,
    VoiceToTextStream = 7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enumeration)]
#[repr(i32)]
pub enum SpawnedEnvelopeKind {
    Unspecified = 0,
    Request = 1,
    Data = 2,
    Complete = 3,
    Cancellation = 4,
    Failure = 5,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpawnedChatMessage {
    #[prost(string, tag = "1")]
    pub role: String,
    #[prost(string, tag = "2")]
    pub content: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpawnedMcpServer {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub url: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpawnedEnvelope {
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    #[prost(enumeration = "SpawnedEnvelopeKind", tag = "2")]
    pub kind: i32,
    #[prost(enumeration = "SpawnedOperation", tag = "3")]
    pub operation: i32,
    #[prost(string, tag = "4")]
    pub request_id: String,
    #[prost(message, repeated, tag = "5")]
    pub messages: Vec<SpawnedChatMessage>,
    #[prost(string, tag = "6")]
    pub text: String,
    #[prost(string, tag = "7")]
    pub model: String,
    #[prost(string, tag = "8")]
    pub media_type: String,
    #[prost(bytes = "vec", tag = "9")]
    pub binary: Vec<u8>,
    #[prost(float, repeated, packed = "true", tag = "10")]
    pub vector: Vec<f32>,
    #[prost(uint64, tag = "11")]
    pub dimensions: u64,
    #[prost(bool, tag = "12")]
    pub normalized: bool,
    #[prost(string, tag = "13")]
    pub provider_id: String,
    #[prost(string, tag = "14")]
    pub language: String,
    #[prost(float, tag = "15")]
    pub confidence: f32,
    #[prost(bool, tag = "16")]
    pub has_confidence: bool,
    #[prost(uint64, tag = "17")]
    pub sequence: u64,
    #[prost(bool, tag = "18")]
    pub is_final: bool,
    #[prost(uint32, tag = "19")]
    pub sample_rate_hz: u32,
    #[prost(string, tag = "20")]
    pub engine_id: String,
    #[prost(string, tag = "21")]
    pub error_message: String,
    #[prost(bytes = "vec", tag = "22")]
    pub metadata_json: Vec<u8>,
    #[prost(bytes = "vec", repeated, tag = "23")]
    pub segments_json: Vec<Vec<u8>>,
    #[prost(string, tag = "24")]
    pub voice_id: String,
    #[prost(message, repeated, tag = "25")]
    pub mcp_servers: Vec<SpawnedMcpServer>,
}

impl SpawnedEnvelope {
    pub fn request(operation: SpawnedOperation, request_id: impl Into<String>) -> Self {
        Self {
            protocol_major: SPAWNED_ENGINE_PROTOCOL_MAJOR,
            kind: SpawnedEnvelopeKind::Request as i32,
            operation: operation as i32,
            request_id: request_id.into(),
            ..Self::default()
        }
    }

    pub fn require_supported_major(&self) -> Result<()> {
        if self.protocol_major == SPAWNED_ENGINE_PROTOCOL_MAJOR {
            return Ok(());
        }
        Err(NeuralError::InvalidValue {
            value: self.protocol_major.to_string(),
            expected: format!("spawned engine protocol major {SPAWNED_ENGINE_PROTOCOL_MAJOR}"),
        })
    }

    pub fn envelope_kind(&self) -> Result<SpawnedEnvelopeKind> {
        SpawnedEnvelopeKind::try_from(self.kind).map_err(|_| NeuralError::MalformedPayload {
            value: self.kind.to_string(),
            expected: "known spawned envelope kind".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_envelope_kind_round_trips() {
        for kind in [
            SpawnedEnvelopeKind::Request,
            SpawnedEnvelopeKind::Data,
            SpawnedEnvelopeKind::Complete,
            SpawnedEnvelopeKind::Cancellation,
            SpawnedEnvelopeKind::Failure,
        ] {
            let mut envelope = SpawnedEnvelope::request(SpawnedOperation::TextToVoice, "request-1");
            envelope.kind = kind as i32;
            envelope.binary = vec![0, 1, 2, 255];
            let decoded = SpawnedEnvelope::decode(envelope.encode_to_vec().as_slice()).unwrap();
            assert_eq!(decoded, envelope);
            assert_eq!(decoded.binary, vec![0, 1, 2, 255]);
        }
    }

    #[test]
    fn unsupported_protocol_major_reports_actual_and_expected_versions() {
        let mut envelope = SpawnedEnvelope::request(SpawnedOperation::LlmComplete, "request-1");
        envelope.protocol_major = 2;

        let error = envelope.require_supported_major().unwrap_err().to_string();

        assert!(error.contains("`2`"));
        assert!(error.contains("protocol major 1"));
    }
}
