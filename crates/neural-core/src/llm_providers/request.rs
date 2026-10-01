use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub messages: Vec<LlmMessage>,
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<LlmMcpServerConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modality_inputs: Vec<LlmModalityInput>,
    #[serde(default, skip_serializing_if = "LlmRequestOptions::is_default")]
    pub options: LlmRequestOptions,
}

/// Optional generation controls. Providers apply what they support and
/// ignore the rest; an all-`None` value means provider defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmRequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<LlmResponseFormat>,
    /// Per-request reasoning effort; currently honored by OpenRouter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<LlmReasoningEffort>,
}

/// Provider-independent reasoning preference, e.g. `LlmReasoningEffort::Low`
/// for an interactive turn. Omit it to retain the provider's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmReasoningEffort {
    Low,
    Medium,
    High,
}

impl LlmRequestOptions {
    /// True when no option is set, so serialization can omit the field.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Requested shape of the final text reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmResponseFormat {
    /// Any single JSON object.
    JsonObject,
    /// A JSON object matching `schema` (JSON Schema).
    JsonSchema {
        name: String,
        schema: Value,
        #[serde(default)]
        strict: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmMcpServerConfig {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmModalityInput {
    pub input_id: String,
    pub kind: LlmModalityInputKind,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmModalityInputKind {
    ImageSnapshot,
    LiveAudioChunk,
    ScreenFrame,
}

impl LlmRequest {
    /// Returns the request model override when provided.
    ///
    /// # Example
    ///
    /// ```
    /// let request = lumvise_neural_core::llm_providers::LlmRequest {
    ///     messages: vec![], stream: false, provider_id: None, model: Some("gpt".into()),
    ///     conversation_id: None, provider_session_id: None,
    ///     mcp_servers: vec![], modality_inputs: vec![],
    ///     options: Default::default(),
    /// };
    /// assert_eq!(request.model_id(), Some("gpt"));
    /// ```
    pub fn model_id(&self) -> Option<&str> {
        self.model
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }
}
