use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineMetadata {
    pub engine_id: String,
    pub model: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticCandidate {
    pub candidate_id: String,
    pub content: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MappingResult {
    pub candidate_id: String,
    pub score: f32,
    pub explanation: String,
    pub metadata: Value,
}
