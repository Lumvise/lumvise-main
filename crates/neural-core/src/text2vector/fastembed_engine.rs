use crate::error::{NeuralError, Result, require_non_empty};
use crate::semantic_mappers::VectorEmbedder;
use crate::text2vector::{Text2VectorRequest, Text2VectorResponse};
use crate::types::EngineMetadata;
use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
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
    model: Mutex<TextEmbedding>,
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
        let vector_engine_id = if config.model_dir.is_some() && pooling == Pooling::Cls {
            // A different embedding space must never reuse persisted mean-pooled vectors.
            format!("{}::bge-cls-v1", config.engine_id)
        } else {
            config.engine_id.clone()
        };
        let model = match config.model_dir.as_deref() {
            Some(model_dir) => local_model(model_dir, config.max_length, pooling)?,
            None => builtin_model(&config)?,
        };
        Ok(Self {
            config,
            vector_engine_id,
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
        require_non_empty(&request.text, "non-empty text to vectorize")?;
        require_requested_model(request.model.as_deref(), &self.config.model_id)?;
        let vector = self.embed_text(&request.text)?;
        Ok(Text2VectorResponse {
            dimensions: vector.len(),
            vector,
            normalized: false,
            metadata: EngineMetadata {
                engine_id: self.vector_engine_id.clone(),
                model: Some(self.config.model_id.clone()),
                metadata: json!({ "backend": "fastembed" }),
            },
        })
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

impl VectorEmbedder for FastEmbedText2VectorEngine {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let mut model = self.model.lock().map_err(|_| NeuralError::ProviderFailed {
            provider_id: self.config.engine_id.clone(),
            message: "FastEmbed model mutex poisoned".to_string(),
        })?;
        let embeddings = model.embed(vec![text.to_string()], None).map_err(|error| {
            NeuralError::ProviderFailed {
                provider_id: self.config.engine_id.clone(),
                message: error.to_string(),
            }
        })?;
        embeddings
            .into_iter()
            .next()
            .filter(|vector| !vector.is_empty())
            .ok_or_else(|| NeuralError::MalformedPayload {
                value: text.to_string(),
                expected: "non-empty FastEmbed vector".to_string(),
            })
    }
}

fn validate_config(config: &FastEmbedText2VectorConfig) -> Result<()> {
    require_non_empty(&config.engine_id, "non-empty FastEmbed engine id")?;
    require_non_empty(&config.model_id, "non-empty FastEmbed model id")
}

fn builtin_model(config: &FastEmbedText2VectorConfig) -> Result<TextEmbedding> {
    let model = resolve_fastembed_model(&config.model_id)?;
    let mut options =
        InitOptions::new(model).with_show_download_progress(config.show_download_progress);
    if let Some(cache_dir) = &config.cache_dir {
        options = options.with_cache_dir(cache_dir.clone());
    }
    if let Some(max_length) = config.max_length {
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
) -> Result<TextEmbedding> {
    let model = UserDefinedEmbeddingModel::new(
        read_model_file(model_dir, "model.onnx")?,
        tokenizer_files(model_dir)?,
    )
    .with_pooling(pooling);
    let options = InitOptionsUserDefined::new().with_max_length(max_length.unwrap_or(512));
    TextEmbedding::try_new_from_user_defined(model, options).map_err(|error| {
        NeuralError::ProviderFailed {
            provider_id: "fastembed-local".to_string(),
            message: error.to_string(),
        }
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
        "mxbai-embed-xsmall-v1" | "mixedbread-ai/mxbai-embed-xsmall-v1" => {
            Ok(EmbeddingModel::MxbaiEmbedLargeV1)
        }
        _ => Err(NeuralError::InvalidValue {
            value: model_id.to_string(),
            expected: "supported FastEmbed model alias".to_string(),
        }),
    }
}
