use crate::types::EngineMetadata;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text2VoiceResponse {
    pub audio: Vec<u8>,
    pub media_type: String,
    pub sample_rate_hz: Option<u32>,
    pub metadata: EngineMetadata,
}
