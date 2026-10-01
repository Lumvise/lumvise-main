use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticMappingResult {
    pub candidate_id: String,
    pub score: f32,
    pub explanation: String,
    pub metadata: Value,
}
