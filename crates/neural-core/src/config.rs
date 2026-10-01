use crate::error::{NeuralError, Result, require_non_empty};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeuralCoreConfig {
    pub providers: Vec<LlmProviderConfig>,
    pub text2voice: Option<EngineConfig>,
    pub voice2text: Option<EngineConfig>,
    pub text2vector: Option<EngineConfig>,
}

impl NeuralCoreConfig {
    /// Validates Neural Core configuration before runtime services are built.
    ///
    /// # Example
    ///
    /// ```
    /// let config = lumvise_neural_core::NeuralCoreConfig {
    ///     providers: vec![], text2voice: None, voice2text: None, text2vector: None,
    /// };
    /// assert!(config.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<()> {
        for provider in &self.providers {
            provider.validate()?;
        }
        validate_engine(&self.text2voice)?;
        validate_engine(&self.voice2text)?;
        validate_engine(&self.text2vector)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    pub provider_id: String,
    pub kind: LlmProviderKind,
    pub model: String,
    pub endpoint: Option<String>,
    pub credential: Option<String>,
    pub completion_concurrency: Option<usize>,
    pub spawn: Option<SpawnConfig>,
}

impl LlmProviderConfig {
    /// Validates one LLM provider config.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind};
    /// let config = LlmProviderConfig {
    ///     provider_id: "local".into(), kind: LlmProviderKind::Local, model: "m".into(),
    ///     endpoint: None, credential: None, completion_concurrency: None, spawn: None,
    /// };
    /// assert!(config.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<()> {
        require_non_empty(&self.provider_id, "non-empty provider id")?;
        require_non_empty(&self.model, "non-empty model id")?;
        if let Some(concurrency) = self.completion_concurrency {
            if concurrency == 0 {
                return Err(NeuralError::InvalidValue {
                    value: concurrency.to_string(),
                    expected: "completion concurrency greater than 0".to_string(),
                });
            }
        }
        match self.kind {
            LlmProviderKind::Local => validate_spawn(self.spawn.as_ref()),
            LlmProviderKind::OpenAiRealtime => validate_api_key_provider(self),
            LlmProviderKind::OpenAiCompatible => validate_endpoint_provider(self),
            LlmProviderKind::Cerebras | LlmProviderKind::OpenRouter | LlmProviderKind::Zai => {
                validate_remote_provider(self)
            }
            LlmProviderKind::Claude | LlmProviderKind::Codex | LlmProviderKind::Gemini => {
                validate_spawn_or_api_key_provider(self)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmProviderKind {
    Cerebras,
    Claude,
    Codex,
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    Gemini,
    OpenAiRealtime,
    OpenRouter,
    Zai,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineConfig {
    pub engine_id: String,
    pub spawn: SpawnConfig,
    pub expected_dimensions: Option<usize>,
}

impl EngineConfig {
    /// Validates a spawned engine config.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{EngineConfig, SpawnConfig};
    /// let config = EngineConfig {
    ///     engine_id: "e".into(),
    ///     spawn: SpawnConfig { command: "echo".into(), args: vec![], timeout_ms: 1000 },
    ///     expected_dimensions: None,
    /// };
    /// assert!(config.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<()> {
        require_non_empty(&self.engine_id, "non-empty engine id")?;
        self.spawn.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnConfig {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

impl SpawnConfig {
    /// Validates a command used by spawned Neural Core adapters.
    ///
    /// # Example
    ///
    /// ```
    /// let spawn = lumvise_neural_core::SpawnConfig {
    ///     command: "echo".into(), args: vec![], timeout_ms: 1000,
    /// };
    /// assert!(spawn.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<()> {
        require_non_empty(&self.command, "non-empty executable command")?;
        if self.timeout_ms == 0 {
            return Err(NeuralError::InvalidValue {
                value: self.timeout_ms.to_string(),
                expected: "timeout greater than 0ms".to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub(crate) fn display_command(&self) -> String {
        PathBuf::from(&self.command).display().to_string()
    }
}

fn validate_engine(config: &Option<EngineConfig>) -> Result<()> {
    match config {
        Some(engine) => engine.validate(),
        None => Ok(()),
    }
}

fn validate_spawn(spawn: Option<&SpawnConfig>) -> Result<()> {
    match spawn {
        Some(config) => config.validate(),
        None => Err(NeuralError::MissingValue {
            value: "spawn".to_string(),
            expected: "local provider spawn config".to_string(),
        }),
    }
}

fn validate_remote_provider(config: &LlmProviderConfig) -> Result<()> {
    require_non_empty(
        config.endpoint.as_deref().unwrap_or_default(),
        "non-empty provider endpoint",
    )?;
    require_non_empty(
        config.credential.as_deref().unwrap_or_default(),
        "non-empty provider credential",
    )
}

// User-owned OpenAI-compatible endpoints (Ollama, vLLM, LM Studio, local
// gateways) legitimately run without any credential, so the API key is
// optional; the endpoint is the only required configuration.
fn validate_endpoint_provider(config: &LlmProviderConfig) -> Result<()> {
    require_non_empty(
        config.endpoint.as_deref().unwrap_or_default(),
        "non-empty provider endpoint",
    )
}

fn validate_api_key_provider(config: &LlmProviderConfig) -> Result<()> {
    require_non_empty(
        config.credential.as_deref().unwrap_or_default(),
        "non-empty provider credential",
    )
}

fn validate_spawn_or_api_key_provider(config: &LlmProviderConfig) -> Result<()> {
    if config.spawn.is_some() {
        return validate_spawn(config.spawn.as_ref());
    }
    require_non_empty(
        config.credential.as_deref().unwrap_or_default(),
        "non-empty provider credential or spawn config",
    )
}
