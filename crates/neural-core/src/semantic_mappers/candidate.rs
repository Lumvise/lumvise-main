use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticMappingCandidate {
    pub candidate_id: String,
    pub content: String,
    pub metadata: Value,
}
