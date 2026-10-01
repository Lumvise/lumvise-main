use crate::types::EngineMetadata;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Voice2TextResponse {
    pub transcript: String,
    pub language: Option<String>,
    pub confidence: Option<f32>,
    pub segments: Vec<Value>,
    pub metadata: EngineMetadata,
}
