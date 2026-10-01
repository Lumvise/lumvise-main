use crate::config::EngineConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text2VectorRuntimeConfig {
    Spawned {
        engine: EngineConfig,
    },
    #[cfg(feature = "fastembed")]
    FastEmbedBuiltIn {
        engine_id: String,
        model_id: String,
    },
    #[cfg(feature = "fastembed")]
    FastEmbedLocalOnnx {
        engine_id: String,
        model_id: String,
        model_dir: std::path::PathBuf,
    },
}

impl Text2VectorRuntimeConfig {
    /// Builds a runtime config for an external text-to-vector executable.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{EngineConfig, SpawnConfig, Text2VectorRuntimeConfig};
    /// let engine = EngineConfig {
    ///     engine_id: "embed".into(),
    ///     spawn: SpawnConfig { command: "echo".into(), args: vec![], timeout_ms: 1000 },
    ///     expected_dimensions: None,
    /// };
    /// let config = Text2VectorRuntimeConfig::spawned(engine);
    /// assert!(matches!(config, Text2VectorRuntimeConfig::Spawned { .. }));
    /// ```
    pub fn spawned(engine: EngineConfig) -> Self {
        Self::Spawned { engine }
    }

    /// Builds a runtime config for a FastEmbed built-in model.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = Text2VectorRuntimeConfig::fastembed_builtin("embed", "bge-m3");
    /// ```
    #[cfg(feature = "fastembed")]
    pub fn fastembed_builtin(engine_id: &str, model_id: &str) -> Self {
        Self::FastEmbedBuiltIn {
            engine_id: engine_id.to_string(),
            model_id: model_id.to_string(),
        }
    }

    /// Builds a runtime config for a local ONNX FastEmbed model directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = Text2VectorRuntimeConfig::fastembed_local_onnx("embed", "bge-m3", model_dir);
    /// ```
    #[cfg(feature = "fastembed")]
    pub fn fastembed_local_onnx(
        engine_id: &str,
        model_id: &str,
        model_dir: std::path::PathBuf,
    ) -> Self {
        Self::FastEmbedLocalOnnx {
            engine_id: engine_id.to_string(),
            model_id: model_id.to_string(),
            model_dir,
        }
    }
}
