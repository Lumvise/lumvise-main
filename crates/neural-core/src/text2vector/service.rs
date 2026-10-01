use crate::Result;
use crate::config::EngineConfig;
#[cfg(feature = "fastembed")]
use crate::text2vector::FastEmbedText2VectorConfig;
#[cfg(feature = "fastembed")]
use crate::text2vector::FastEmbedText2VectorEngine;
use crate::text2vector::spawned_engine::SpawnedText2VectorEngine;
use crate::text2vector::{Text2VectorRequest, Text2VectorResponse, Text2VectorRuntimeConfig};

pub struct Text2VectorService {
    backend: Text2VectorBackend,
}

impl Text2VectorService {
    /// Creates a text-to-vector service backed by a spawned engine.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{EngineConfig, SpawnConfig, Text2VectorService};
    /// let config = EngineConfig {
    ///     engine_id: "embed".into(),
    ///     spawn: SpawnConfig { command: "echo".into(), args: vec![], timeout_ms: 1000 },
    ///     expected_dimensions: None,
    /// };
    /// assert!(Text2VectorService::new(config).is_ok());
    /// ```
    pub fn new(config: EngineConfig) -> Result<Self> {
        Self::from_runtime_config(Text2VectorRuntimeConfig::spawned(config))
    }

    /// Creates a text-to-vector service from the selected runtime backend.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_neural_core::{EngineConfig, SpawnConfig, Text2VectorRuntimeConfig, Text2VectorService};
    /// let engine = EngineConfig {
    ///     engine_id: "embed".into(),
    ///     spawn: SpawnConfig { command: "echo".into(), args: vec![], timeout_ms: 1000 },
    ///     expected_dimensions: None,
    /// };
    /// let config = Text2VectorRuntimeConfig::spawned(engine);
    /// assert!(Text2VectorService::from_runtime_config(config).is_ok());
    /// ```
    pub fn from_runtime_config(config: Text2VectorRuntimeConfig) -> Result<Self> {
        Ok(Self {
            backend: Text2VectorBackend::from_runtime_config(config)?,
        })
    }

    /// Creates a FastEmbed-backed text-to-vector service.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let service = Text2VectorService::fastembed_builtin("embed", "all-minilm-l6-v2")?;
    /// ```
    #[cfg(feature = "fastembed")]
    pub fn fastembed_builtin(engine_id: &str, model_id: &str) -> Result<Self> {
        Self::from_runtime_config(Text2VectorRuntimeConfig::fastembed_builtin(
            engine_id, model_id,
        ))
    }

    pub fn embed(&self, request: &Text2VectorRequest) -> Result<Text2VectorResponse> {
        self.backend.embed(request)
    }
}

enum Text2VectorBackend {
    Spawned(SpawnedText2VectorEngine),
    #[cfg(feature = "fastembed")]
    FastEmbed(Box<FastEmbedText2VectorEngine>),
}

impl Text2VectorBackend {
    fn from_runtime_config(config: Text2VectorRuntimeConfig) -> Result<Self> {
        match config {
            Text2VectorRuntimeConfig::Spawned { engine } => Self::spawned(engine),
            #[cfg(feature = "fastembed")]
            Text2VectorRuntimeConfig::FastEmbedBuiltIn {
                engine_id,
                model_id,
            } => Self::fastembed_builtin(&engine_id, &model_id),
            #[cfg(feature = "fastembed")]
            Text2VectorRuntimeConfig::FastEmbedLocalOnnx {
                engine_id,
                model_id,
                model_dir,
            } => Self::fastembed_local(&engine_id, &model_id, model_dir),
        }
    }

    fn spawned(engine: EngineConfig) -> Result<Self> {
        Ok(Self::Spawned(SpawnedText2VectorEngine::new(engine)?))
    }

    #[cfg(feature = "fastembed")]
    fn fastembed_builtin(engine_id: &str, model_id: &str) -> Result<Self> {
        let config = FastEmbedText2VectorConfig::builtin(engine_id, model_id);
        Ok(Self::FastEmbed(Box::new(FastEmbedText2VectorEngine::new(
            config,
        )?)))
    }

    #[cfg(feature = "fastembed")]
    fn fastembed_local(
        engine_id: &str,
        model_id: &str,
        model_dir: std::path::PathBuf,
    ) -> Result<Self> {
        let config = FastEmbedText2VectorConfig::local_onnx(engine_id, model_id, model_dir);
        Ok(Self::FastEmbed(Box::new(FastEmbedText2VectorEngine::new(
            config,
        )?)))
    }

    fn embed(&self, request: &Text2VectorRequest) -> Result<Text2VectorResponse> {
        match self {
            Self::Spawned(engine) => engine.embed(request),
            #[cfg(feature = "fastembed")]
            Self::FastEmbed(engine) => engine.embed(request),
        }
    }
}

impl lumvise_db_core::ArtifactTextVectorizer for Text2VectorService {
    fn vectorize_artifact_text(
        &self,
        text: &str,
    ) -> lumvise_db_core::Result<lumvise_db_core::ArtifactTextVector> {
        let response = self
            .embed(&Text2VectorRequest {
                text: text.to_string(),
                model: None,
            })
            .map_err(|error| lumvise_db_core::DbError::vectorizer(error.to_string()))?;
        Ok(lumvise_db_core::ArtifactTextVector {
            engine_id: response.metadata.engine_id,
            model: response.metadata.model,
            dimensions: response.dimensions,
            vector: response.vector,
            normalized: response.normalized,
            metadata: response.metadata.metadata,
        })
    }
}
