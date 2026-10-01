use crate::config::EngineConfig;
use crate::error::{NeuralError, Result, require_non_empty};
use crate::semantic_mappers::lexical;
use crate::semantic_mappers::model_spec::ModelSpecRanker;
use crate::semantic_mappers::{
    SemanticMapperMode, SemanticMappingCandidate, SemanticMappingResult, VectorEmbedder,
};
use crate::text2vector::{Text2VectorRequest, Text2VectorService};
use lumvise_db_core::{SemanticOperation, SemanticPersistence, SemanticResult};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub struct SemanticMapperService {
    embedder: Option<Arc<dyn VectorEmbedder>>,
    arch_router: Option<ModelSpecRanker>,
    model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticMappingRequest {
    pub mode: SemanticMapperMode,
    pub source: String,
    pub candidates: Vec<SemanticMappingCandidate>,
    pub model: Option<String>,
}

pub enum SemanticMapperRuntimeConfig {
    Lexical,
    ArchRouter {
        model_dir: PathBuf,
    },
    Text2Vector {
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
        model_dir: PathBuf,
    },
}

impl SemanticMapperService {
    /// Creates a semantic mapper using a vector embedder for model-backed modes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # use std::sync::Arc;
    /// # use lumvise_neural_core::semantic_mappers::{SemanticMapperService, VectorEmbedder};
    /// # let embedder: Arc<dyn VectorEmbedder> = build_embedder();
    /// let mapper = SemanticMapperService::new(embedder);
    /// assert!(mapper.is_ok());
    /// ```
    pub fn new(embedder: Arc<dyn VectorEmbedder>) -> Result<Self> {
        Ok(Self {
            embedder: Some(embedder),
            arch_router: None,
            model_id: None,
        })
    }

    /// Creates a mapper that only uses lexical scoring.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mapper = SemanticMapperService::lexical();
    /// ```
    pub fn lexical() -> Self {
        Self {
            embedder: None,
            arch_router: None,
            model_id: Some("lexical".to_string()),
        }
    }

    /// Creates a mapper from runtime configuration.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mapper = SemanticMapperService::from_runtime_config(SemanticMapperRuntimeConfig::Lexical)?;
    /// ```
    pub fn from_runtime_config(config: SemanticMapperRuntimeConfig) -> Result<Self> {
        match config {
            SemanticMapperRuntimeConfig::Lexical => Ok(Self::lexical()),
            SemanticMapperRuntimeConfig::ArchRouter { model_dir } => {
                Self::from_arch_router(model_dir)
            }
            SemanticMapperRuntimeConfig::Text2Vector { engine } => Self::from_text2vector(engine),
            #[cfg(feature = "fastembed")]
            SemanticMapperRuntimeConfig::FastEmbedBuiltIn {
                engine_id,
                model_id,
            } => Self::from_fastembed_builtin(&engine_id, &model_id),
            #[cfg(feature = "fastembed")]
            SemanticMapperRuntimeConfig::FastEmbedLocalOnnx {
                engine_id,
                model_id,
                model_dir,
            } => Self::from_fastembed_local(&engine_id, &model_id, model_dir),
        }
    }

    /// Ranks candidates for the selected semantic mapper mode.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let results = mapper.map_candidates(SemanticMapperMode::Lexical, "policy", &candidates)?;
    /// ```
    pub fn map_candidates(
        &self,
        mode: SemanticMapperMode,
        source: &str,
        candidates: &[SemanticMappingCandidate],
    ) -> Result<Vec<SemanticMappingResult>> {
        self.map_candidates_with_model(mode, source, candidates, None)
    }

    pub fn map_request(
        &self,
        request: &SemanticMappingRequest,
    ) -> Result<Vec<SemanticMappingResult>> {
        self.map_candidates_with_model(
            request.mode,
            &request.source,
            &request.candidates,
            request.model.as_deref(),
        )
    }

    pub fn map_candidates_with_model(
        &self,
        mode: SemanticMapperMode,
        source: &str,
        candidates: &[SemanticMappingCandidate],
        model: Option<&str>,
    ) -> Result<Vec<SemanticMappingResult>> {
        require_non_empty(source, "non-empty source content")?;
        self.require_requested_model(model)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        if matches!(mode, SemanticMapperMode::ArchRouter)
            && let Some(ranker) = &self.arch_router
        {
            return Ok(ranker.rank(source, candidates));
        }
        if mode.requires_embeddings() {
            return self.map_with_embeddings(mode, source, candidates);
        }
        Ok(lexical::rank(source, candidates))
    }

    /// Reads DB artifacts through DB Core and ranks them as semantic candidates.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let results = mapper.map_db_artifacts(SemanticMapperMode::Lexical, "policy", &db, "node-1")?;
    /// ```
    pub fn map_db_artifacts(
        &self,
        mode: SemanticMapperMode,
        source: &str,
        persistence: &dyn SemanticPersistence,
        semantic_element_id: &str,
    ) -> Result<Vec<SemanticMappingResult>> {
        let candidates = db_artifact_candidates(persistence, semantic_element_id)?;
        self.map_candidates(mode, source, &candidates)
    }

    fn from_arch_router(model_dir: PathBuf) -> Result<Self> {
        let ranker = ModelSpecRanker::from_dir(model_dir)?;
        let model_id = ranker.model_id().to_string();
        Ok(Self {
            embedder: None,
            arch_router: Some(ranker),
            model_id: Some(model_id),
        })
    }

    fn from_text2vector(engine: EngineConfig) -> Result<Self> {
        let model_id = engine.engine_id.clone();
        let mut service = Self::new(Arc::new(Text2VectorService::new(engine)?))?;
        service.model_id = Some(model_id);
        Ok(service)
    }

    #[cfg(feature = "fastembed")]
    fn from_fastembed_builtin(engine_id: &str, model_id: &str) -> Result<Self> {
        let config = crate::text2vector::FastEmbedText2VectorConfig::builtin(engine_id, model_id);
        let mut service = Self::new(Arc::new(
            crate::text2vector::FastEmbedText2VectorEngine::new(config)?,
        ))?;
        service.model_id = Some(model_id.to_string());
        Ok(service)
    }

    #[cfg(feature = "fastembed")]
    fn from_fastembed_local(engine_id: &str, model_id: &str, model_dir: PathBuf) -> Result<Self> {
        let config = crate::text2vector::FastEmbedText2VectorConfig::local_onnx(
            engine_id, model_id, model_dir,
        );
        let mut service = Self::new(Arc::new(
            crate::text2vector::FastEmbedText2VectorEngine::new(config)?,
        ))?;
        service.model_id = Some(model_id.to_string());
        Ok(service)
    }

    fn map_with_embeddings(
        &self,
        mode: SemanticMapperMode,
        source: &str,
        candidates: &[SemanticMappingCandidate],
    ) -> Result<Vec<SemanticMappingResult>> {
        let embedder = self.embedder()?;
        let source_vector = embedder.embed_text(source)?;
        let mut results = self.embedding_results(mode, candidates, &source_vector)?;
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });
        Ok(results)
    }

    fn embedding_results(
        &self,
        mode: SemanticMapperMode,
        candidates: &[SemanticMappingCandidate],
        source_vector: &[f32],
    ) -> Result<Vec<SemanticMappingResult>> {
        candidates
            .iter()
            .map(|candidate| self.embedding_result(mode, candidate, source_vector))
            .collect()
    }

    fn embedding_result(
        &self,
        mode: SemanticMapperMode,
        candidate: &SemanticMappingCandidate,
        source_vector: &[f32],
    ) -> Result<SemanticMappingResult> {
        let candidate_vector = self.embedder()?.embed_text(&candidate.content)?;
        let score = cosine_similarity(source_vector, &candidate_vector)?;
        Ok(SemanticMappingResult {
            candidate_id: candidate.candidate_id.clone(),
            score,
            explanation: format!("{mode:?} cosine similarity"),
            metadata: json!({ "mode": format!("{mode:?}") }),
        })
    }

    fn embedder(&self) -> Result<&dyn VectorEmbedder> {
        self.embedder
            .as_deref()
            .ok_or_else(|| NeuralError::MissingValue {
                value: "vector embedder".to_string(),
                expected: "semantic mapper configured with Text2Vector or FastEmbed runtime"
                    .to_string(),
            })
    }

    fn require_requested_model(&self, requested: Option<&str>) -> Result<()> {
        let Some(requested) = requested.filter(|value| !value.trim().is_empty()) else {
            return Ok(());
        };
        if self.model_id.as_deref() == Some(requested) {
            return Ok(());
        }
        Err(NeuralError::InvalidValue {
            value: requested.to_string(),
            expected: format!("configured semantic mapper model {:?}", self.model_id),
        })
    }
}

impl VectorEmbedder for Text2VectorService {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self
            .embed(&Text2VectorRequest {
                text: text.to_string(),
                model: None,
            })?
            .vector)
    }
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Result<f32> {
    if left.len() != right.len() {
        return Err(NeuralError::InvalidValue {
            value: format!("{} vs {}", left.len(), right.len()),
            expected: "vectors with equal dimensions".to_string(),
        });
    }
    let dot_product: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        return Ok(0.0);
    }
    Ok(dot_product / (left_norm * right_norm))
}

fn db_artifact_candidates(
    persistence: &dyn SemanticPersistence,
    semantic_element_id: &str,
) -> Result<Vec<SemanticMappingCandidate>> {
    let result = persistence.execute(
        SemanticOperation::ArtifactsForElementWithInheritance {
            semantic_element_id: semantic_element_id.into(),
        },
        &InvocationControl::sixty_seconds(),
    )?;
    let SemanticResult::Artifacts(artifacts) = result else {
        return Err(NeuralError::DbCore(format!(
            "artifact lookup returned unexpected semantic result: {result:?}"
        )));
    };
    Ok(artifacts
        .into_iter()
        .filter_map(artifact_candidate)
        .collect())
}

fn artifact_candidate(
    artifact: lumvise_db_core::SemanticArtifact,
) -> Option<SemanticMappingCandidate> {
    let content = artifact.searchable_text.or(artifact.content)?;
    Some(SemanticMappingCandidate {
        candidate_id: artifact.artifact_id,
        content,
        metadata: artifact.metadata,
    })
}
