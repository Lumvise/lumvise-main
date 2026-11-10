use crate::Result;
use crate::config::EngineConfig;
#[cfg(feature = "fastembed")]
use crate::text2vector::FastEmbedText2VectorConfig;
#[cfg(feature = "fastembed")]
use crate::text2vector::FastEmbedText2VectorEngine;
use crate::text2vector::spawned_engine::SpawnedText2VectorEngine;
use crate::text2vector::{
    Text2VectorRequest, Text2VectorResponse, Text2VectorRuntimeConfig, TextChunkPlan,
    TextChunkPlanRequest, TextChunkPlanner,
};
use lumvise_db_core::TextEmbeddingTask;

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
        self.embed_task(request, TextEmbeddingTask::Document)
    }

    pub fn embed_task(
        &self,
        request: &Text2VectorRequest,
        task: TextEmbeddingTask,
    ) -> Result<Text2VectorResponse> {
        self.backend.embed_task(request, task)
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

    fn embed_task(
        &self,
        request: &Text2VectorRequest,
        task: TextEmbeddingTask,
    ) -> Result<Text2VectorResponse> {
        match self {
            // shortcut: spawned engines have no task contract, so the task is dropped there; revisit when a spawned engine needs asymmetric prefixes.
            Self::Spawned(engine) => engine.embed(request),
            #[cfg(feature = "fastembed")]
            Self::FastEmbed(engine) => engine.embed_task(request, task),
        }
    }
}

impl lumvise_db_core::ArtifactTextVectorizer for Text2VectorService {
    fn vectorize_artifact_text(
        &self,
        text: &str,
        task: TextEmbeddingTask,
    ) -> lumvise_db_core::Result<lumvise_db_core::ArtifactTextVector> {
        let response = self
            .embed_task(
                &Text2VectorRequest {
                    text: text.to_string(),
                    model: None,
                },
                task,
            )
            .map_err(|error| lumvise_db_core::DbError::vectorizer(error.to_string()))?;
        Ok(stored_vector(response))
    }

    fn vectorize_artifact_texts(
        &self,
        texts: &[&str],
        task: TextEmbeddingTask,
    ) -> lumvise_db_core::Result<Vec<lumvise_db_core::ArtifactTextVector>> {
        match &self.backend {
            Text2VectorBackend::Spawned(_) => texts
                .iter()
                .map(|text| self.vectorize_artifact_text(text, task))
                .collect(),
            #[cfg(feature = "fastembed")]
            Text2VectorBackend::FastEmbed(engine) => engine
                .embed_batch(texts, task)
                .map(|responses| responses.into_iter().map(stored_vector).collect())
                .map_err(|error| lumvise_db_core::DbError::vectorizer(error.to_string())),
        }
    }
}

fn stored_vector(response: Text2VectorResponse) -> lumvise_db_core::ArtifactTextVector {
    lumvise_db_core::ArtifactTextVector {
        engine_id: response.metadata.engine_id,
        model: response.metadata.model,
        dimensions: response.dimensions,
        vector: response.vector,
        normalized: response.normalized,
        metadata: response.metadata.metadata,
    }
}

impl TextChunkPlanner for Text2VectorService {
    fn embedding_profile_fingerprint(&self) -> Result<String> {
        match &self.backend {
            Text2VectorBackend::Spawned(_) => Err(unsupported_chunk_planning()),
            #[cfg(feature = "fastembed")]
            Text2VectorBackend::FastEmbed(engine) => engine.embedding_profile_fingerprint(),
        }
    }

    fn plan_text_chunks(&self, request: &TextChunkPlanRequest) -> Result<TextChunkPlan> {
        match &self.backend {
            Text2VectorBackend::Spawned(_) => Err(unsupported_chunk_planning()),
            #[cfg(feature = "fastembed")]
            Text2VectorBackend::FastEmbed(engine) => engine.plan_text_chunks(request),
        }
    }
}

fn unsupported_chunk_planning() -> crate::NeuralError {
    crate::NeuralError::InvalidValue {
        value: "spawned embedding backend".into(),
        expected: "owned local tokenizer and model assets for supported token planning/profile attestation".into(),
    }
}

#[cfg(test)]
mod chunk_backend_tests {
    use super::*;
    #[test]
    fn spawned_service_explicitly_refuses_profile_and_chunk_planning() {
        let service = Text2VectorService::new(EngineConfig {
            engine_id: "spawned".into(),
            spawn: crate::SpawnConfig {
                command: "echo".into(),
                args: vec![],
                timeout_ms: 1000,
            },
            expected_dimensions: None,
        })
        .unwrap();
        assert!(service.embedding_profile_fingerprint().is_err());
        let request = TextChunkPlanRequest {
            text: "private source".into(),
            task: TextEmbeddingTask::Document,
            max_tokens: 1024,
        };
        let error = service.plan_text_chunks(&request).unwrap_err();
        assert!(error.to_string().contains("owned local tokenizer"));
        assert!(!error.to_string().contains("private source"));
    }
}
