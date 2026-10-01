use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub provider_id: String,
    pub model: String,
    pub content: String,
    pub metadata: Value,
}
