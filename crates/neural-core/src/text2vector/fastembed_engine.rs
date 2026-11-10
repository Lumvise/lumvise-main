mod batch;
#[cfg(test)]
mod cached_planning_tests;
mod chunk_planning;
mod profile_identity;
mod token_ranges;

use crate::error::{NeuralError, Result, require_non_empty};
use crate::semantic_mappers::VectorEmbedder;
use crate::text2vector::{
    Text2VectorRequest, Text2VectorResponse, TextChunkPlan, TextChunkPlanRequest, TextChunkPlanner,
};
use crate::types::EngineMetadata;
use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
use lumvise_db_core::TextEmbeddingTask;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct FastEmbedText2VectorConfig {
    pub engine_id: String,
    pub model_id: String,
    pub model_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub max_length: Option<usize>,
    pub show_download_progress: bool,
}

pub struct FastEmbedText2VectorEngine {
    config: FastEmbedText2VectorConfig,
    vector_engine_id: String,
    profile: TextProfile,
    profile_fingerprint: Option<String>,
    model: Mutex<TextEmbedding>,
}

/// Per-model text preparation: task prefixes, context length and optional Matryoshka output size.
struct TextProfile {
    document_prefix: &'static str,
    query_prefix: &'static str,
    max_length: Option<usize>,
    output_dimensions: Option<usize>,
}

const PLAIN_TEXT: TextProfile = TextProfile {
    document_prefix: "",
    query_prefix: "",
    max_length: None,
    output_dimensions: None,
};

const NOMIC_TEXT: TextProfile = TextProfile {
    document_prefix: "search_document: ",
    query_prefix: "search_query: ",
    max_length: Some(2048),
    output_dimensions: Some(512),
};

fn text_profile(model_id: &str) -> TextProfile {
    match resolve_fastembed_model(model_id) {
        Ok(EmbeddingModel::NomicEmbedTextV15) => NOMIC_TEXT,
        _ => PLAIN_TEXT,
    }
}

impl TextProfile {
    fn prepare(&self, text: &str, task: TextEmbeddingTask) -> String {
        let prefix = match task {
            TextEmbeddingTask::Document => self.document_prefix,
            TextEmbeddingTask::Query => self.query_prefix,
        };
        format!("{prefix}{text}")
    }

    /// Matryoshka cut: layer-norm the full output, keep the leading dimensions, then L2-normalize.
    fn finish(&self, mut vector: Vec<f32>) -> Result<Vec<f32>> {
        let Some(dimensions) = self.output_dimensions else {
            return Ok(vector);
        };
        if vector.len() < dimensions {
            return Err(NeuralError::MalformedPayload {
                value: vector.len().to_string(),
                expected: format!("FastEmbed vector of at least {dimensions} dimensions"),
            });
        }
        layer_norm(&mut vector);
        vector.truncate(dimensions);
        l2_normalize(&mut vector);
        Ok(vector)
    }
}

fn layer_norm(values: &mut [f32]) {
    let count = values.len() as f32;
    let mean = values.iter().sum::<f32>() / count;
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f32>()
        / count;
    let scale = (variance + 1e-5).sqrt();
    for value in values.iter_mut() {
        *value = (*value - mean) / scale;
    }
}

fn l2_normalize(values: &mut [f32]) {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in values.iter_mut() {
            *value /= norm;
        }
    }
}

impl FastEmbedText2VectorConfig {
    /// Builds config for a FastEmbed model downloaded or resolved by FastEmbed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = FastEmbedText2VectorConfig::builtin("embed", "bge-m3");
    /// ```
    pub fn builtin(engine_id: &str, model_id: &str) -> Self {
        Self {
            engine_id: engine_id.to_string(),
            model_id: model_id.to_string(),
            model_dir: None,
            cache_dir: None,
            max_length: None,
            show_download_progress: false,
        }
    }

    /// Builds config for a local ONNX FastEmbed model directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = FastEmbedText2VectorConfig::local_onnx("embed", "bge-m3", model_dir);
    /// ```
    pub fn local_onnx(engine_id: &str, model_id: &str, model_dir: PathBuf) -> Self {
        Self {
            engine_id: engine_id.to_string(),
            model_id: model_id.to_string(),
            model_dir: Some(model_dir),
            cache_dir: None,
            max_length: None,
            show_download_progress: false,
        }
    }
}

impl FastEmbedText2VectorEngine {
    /// Creates a FastEmbed-backed text-to-vector engine.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use lumvise_neural_core::text2vector::{FastEmbedText2VectorConfig, FastEmbedText2VectorEngine};
    /// let config = FastEmbedText2VectorConfig::builtin("embed", "all-minilm-l6-v2");
    /// let engine = FastEmbedText2VectorEngine::new(config)?;
    /// # Ok::<(), lumvise_neural_core::NeuralError>(())
    /// ```
    pub fn new(config: FastEmbedText2VectorConfig) -> Result<Self> {
        validate_config(&config)?;
        let pooling = local_pooling(&config.model_id);
        let mut vector_engine_id = if config.model_dir.is_some() && pooling == Pooling::Cls {
            // A different embedding space must never reuse persisted mean-pooled vectors.
            format!("{}::bge-cls-v1", config.engine_id)
        } else {
            config.engine_id.clone()
        };
        let profile = text_profile(&config.model_id);
        let max_length = config.max_length.or(profile.max_length);
        let (model, assets) = match config.model_dir.as_deref() {
            Some(model_dir) => {
                let (model, digest) = local_model(model_dir, max_length, pooling.clone())?;
                (model, Some(digest))
            }
            None => (builtin_model(&config, max_length)?, None),
        };
        let configured_tokenizer =
            model
                .tokenizer
                .to_string(false)
                .map_err(|_| NeuralError::MalformedPayload {
                    value: config.model_id.clone(),
                    expected: "serializable loaded tokenizer configuration".into(),
                })?;
        let profile_fingerprint = assets.map(|assets| {
            profile_identity::profile_digest(
                &config,
                &profile,
                pooling,
                &assets,
                &configured_tokenizer,
            )
        });
        if let Some(fingerprint) = &profile_fingerprint {
            vector_engine_id = format!("{vector_engine_id}::profile-{fingerprint}");
        }
        Ok(Self {
            config,
            vector_engine_id,
            profile,
            profile_fingerprint,
            model: Mutex::new(model),
        })
    }

    /// Embeds one text request through the configured FastEmbed model.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let response = engine.embed(&Text2VectorRequest { text: "policy".to_string() })?;
    /// ```
    pub fn embed(&self, request: &Text2VectorRequest) -> Result<Text2VectorResponse> {
        self.embed_task(request, TextEmbeddingTask::Document)
    }

    /// Embeds one text request as either indexed content or a search query.
    pub fn embed_task(
        &self,
        request: &Text2VectorRequest,
        task: TextEmbeddingTask,
    ) -> Result<Text2VectorResponse> {
        require_non_empty(&request.text, "non-empty text to vectorize")?;
        require_requested_model(request.model.as_deref(), &self.config.model_id)?;
        let vector = self.embed_text_for_task(&request.text, task)?;
        Ok(self.vector_response(vector))
    }

    pub(super) fn embed_batch(
        &self,
        texts: &[&str],
        task: TextEmbeddingTask,
    ) -> Result<Vec<Text2VectorResponse>> {
        self.embed_text_batch(texts, task).map(|vectors| {
            vectors
                .into_iter()
                .map(|vector| self.vector_response(vector))
                .collect()
        })
    }

    fn vector_response(&self, vector: Vec<f32>) -> Text2VectorResponse {
        Text2VectorResponse {
            dimensions: vector.len(),
            vector,
            normalized: false,
            metadata: EngineMetadata {
                engine_id: self.vector_engine_id.clone(),
                model: Some(self.config.model_id.clone()),
                metadata: json!({ "backend": "fastembed" }),
            },
        }
    }
}

fn require_requested_model(requested: Option<&str>, configured: &str) -> Result<()> {
    let Some(requested) = requested.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    if requested == configured {
        return Ok(());
    }
    Err(NeuralError::InvalidValue {
        value: requested.to_string(),
        expected: format!("loaded FastEmbed model {configured}"),
    })
}

impl FastEmbedText2VectorEngine {
    fn embed_text_for_task(&self, text: &str, task: TextEmbeddingTask) -> Result<Vec<f32>> {
        self.embed_text_batch(&[text], task)?
            .pop()
            .ok_or_else(|| NeuralError::MalformedPayload {
                value: "0 vectors".into(),
                expected: "one FastEmbed vector".into(),
            })
    }
}

impl VectorEmbedder for FastEmbedText2VectorEngine {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_text_for_task(text, TextEmbeddingTask::Document)
    }
}

fn validate_config(config: &FastEmbedText2VectorConfig) -> Result<()> {
    require_non_empty(&config.engine_id, "non-empty FastEmbed engine id")?;
    require_non_empty(&config.model_id, "non-empty FastEmbed model id")
}

fn builtin_model(
    config: &FastEmbedText2VectorConfig,
    max_length: Option<usize>,
) -> Result<TextEmbedding> {
    let model = resolve_fastembed_model(&config.model_id)?;
    let mut options =
        InitOptions::new(model).with_show_download_progress(config.show_download_progress);
    if let Some(cache_dir) = &config.cache_dir {
        options = options.with_cache_dir(cache_dir.clone());
    }
    if let Some(max_length) = max_length {
        options = options.with_max_length(max_length);
    }
    TextEmbedding::try_new(options).map_err(|error| NeuralError::ProviderFailed {
        provider_id: config.engine_id.clone(),
        message: error.to_string(),
    })
}

fn local_pooling(model_id: &str) -> Pooling {
    match resolve_fastembed_model(model_id) {
        Ok(EmbeddingModel::BGESmallENV15 | EmbeddingModel::BGEM3) => Pooling::Cls,
        _ => Pooling::Mean,
    }
}

fn local_model(
    model_dir: &Path,
    max_length: Option<usize>,
    pooling: Pooling,
) -> Result<(TextEmbedding, String)> {
    let weights = read_model_file(model_dir, "model.onnx")?;
    let tokenizer = tokenizer_files(model_dir)?;
    let digest = profile_identity::local_asset_digest(&weights, &tokenizer);
    let model = UserDefinedEmbeddingModel::new(weights, tokenizer).with_pooling(pooling);
    let options = InitOptionsUserDefined::new().with_max_length(max_length.unwrap_or(512));
    TextEmbedding::try_new_from_user_defined(model, options)
        .map(|model| (model, digest))
        .map_err(|error| NeuralError::ProviderFailed {
            provider_id: "fastembed-local".to_string(),
            message: error.to_string(),
        })
}

fn tokenizer_files(model_dir: &Path) -> Result<TokenizerFiles> {
    Ok(TokenizerFiles {
        tokenizer_file: read_model_file(model_dir, "tokenizer.json")?,
        config_file: read_model_file(model_dir, "config.json")?,
        special_tokens_map_file: read_model_file(model_dir, "special_tokens_map.json")?,
        tokenizer_config_file: read_model_file(model_dir, "tokenizer_config.json")?,
    })
}

fn read_model_file(model_dir: &Path, file_name: &str) -> Result<Vec<u8>> {
    let path = model_dir.join(file_name);
    std::fs::read(&path).map_err(|source| NeuralError::Io {
        value: path.display().to_string(),
        expected: format!("FastEmbed local ONNX model file {file_name}"),
        source,
    })
}

fn resolve_fastembed_model(model_id: &str) -> Result<EmbeddingModel> {
    let normalized = model_id.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "all-minilm-l6-v2" | "all-minilm-l6-v2::default" => Ok(EmbeddingModel::AllMiniLML6V2),
        "sentence-transformers/all-minilm-l6-v2" => Ok(EmbeddingModel::AllMiniLML6V2),
        "bge-small-en-v1.5" | "bgesmallenv15" => Ok(EmbeddingModel::BGESmallENV15),
        "sentence-transformers/bge-small-en-v1.5" | "baai/bge-small-en-v1.5" => {
            Ok(EmbeddingModel::BGESmallENV15)
        }
        "bge-m3" | "baai/bge-m3" => Ok(EmbeddingModel::BGEM3),
        "nomic-embed-text-v1.5" | "nomic-ai/nomic-embed-text-v1.5" => {
            Ok(EmbeddingModel::NomicEmbedTextV15)
        }
        "mxbai-embed-xsmall-v1" | "mixedbread-ai/mxbai-embed-xsmall-v1" => {
            Ok(EmbeddingModel::MxbaiEmbedLargeV1)
        }
        _ => Err(NeuralError::InvalidValue {
            value: model_id.to_string(),
            expected: "supported FastEmbed model alias".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_norm_matches_hand_computed_values() {
        // Mean 2.5 and population variance 1.25 give offsets -1.5, -0.5, 0.5, 1.5 divided by sqrt(1.25).
        let mut values = [1.0, 2.0, 3.0, 4.0];
        layer_norm(&mut values);
        let expected = [-1.5, -0.5, 0.5, 1.5].map(|offset: f32| offset / 1.25f32.sqrt());
        for (actual, expected) in values.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
        }
    }

    #[test]
    fn nomic_output_is_layer_normed_cut_to_512_and_unit_norm() {
        // Mean 1/3 and variance 8/9 over all 768 values: +1 maps to 1/√2 and -1 maps to -√2.
        // The 512 kept values hold 256 of each, so their squared sum is 640.
        let input: Vec<f32> = (0..768)
            .map(|index| {
                if index < 512 && index % 2 == 1 {
                    -1.0
                } else {
                    1.0
                }
            })
            .collect();

        let output = NOMIC_TEXT.finish(input).expect("768-dimension vector");

        assert_eq!(output.len(), 512);
        let norm = output.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm {norm}");
        let even = 1.0 / 1280f32.sqrt();
        let odd = -1.0 / 320f32.sqrt();
        for (index, value) in output.iter().enumerate() {
            let expected = if index % 2 == 0 { even } else { odd };
            assert!(
                (value - expected).abs() < 1e-6,
                "index {index}: {value} != {expected}"
            );
        }
    }

    #[test]
    fn nomic_prefixes_follow_task_for_both_aliases() {
        for model_id in ["nomic-embed-text-v1.5", "nomic-ai/nomic-embed-text-v1.5"] {
            let profile = text_profile(model_id);
            assert_eq!(
                profile.prepare("notes", TextEmbeddingTask::Document),
                "search_document: notes"
            );
            assert_eq!(
                profile.prepare("notes", TextEmbeddingTask::Query),
                "search_query: notes"
            );
        }
    }

    #[test]
    fn other_models_embed_raw_text_for_every_task() {
        let profile = text_profile("all-minilm-l6-v2");
        assert_eq!(
            profile.prepare("notes", TextEmbeddingTask::Document),
            "notes"
        );
        assert_eq!(profile.prepare("notes", TextEmbeddingTask::Query), "notes");
        assert!(profile.output_dimensions.is_none());
    }
}
