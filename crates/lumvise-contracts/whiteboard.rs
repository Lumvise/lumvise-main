use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canvas data returned by the frontend canvas capability.
///
/// The revision is the stable concurrency header. The remaining fields are
/// canvas-provider data and are preserved verbatim as an extension payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantCanvasV2 {
    pub revision: Option<u64>,
    #[serde(flatten)]
    pub extension: std::collections::BTreeMap<String, Value>,
}

/// Optional Assistant session selection for a canvas read.
///
/// When omitted, the HTTP adapter resolves it from the actual active
/// Assistant state; it never synthesizes an identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetAssistantCanvasRequestV2 {
    pub session_id: Option<String>,
}

/// Canvas snapshot read through the Assistant-scoped canvas MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantCanvasResponseV2 {
    pub session_id: String,
    pub canvas: AssistantCanvasV2,
}

/// Update payload accepted by the Assistant-scoped canvas MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateAssistantCanvasRequestV2 {
    pub session_id: Option<String>,
    pub canvas_patch: Option<Value>,
    pub elements: Option<Vec<Value>>,
    pub agent_elements: Option<Vec<Value>>,
    pub export_scene: Option<Value>,
    pub app_state: Option<Value>,
    pub agent_message: Option<String>,
    pub agent_status: Option<String>,
}
