use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmStreamEvent {
    Session { provider_session_id: String },
    ContentDelta { text: String },
    FinalText { text: String },
    Complete,
    Error { message: String },
    Cancelled,
}
