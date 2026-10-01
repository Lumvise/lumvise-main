use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralRuntimeStatus {
    pub owner: String,
    pub available: bool,
    pub active_backend: NeuralBackend,
    pub loaded_models: Vec<NeuralModelStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeuralBackend {
    None,
    Lexical,
    FastEmbed,
    OnnxRuntime,
    Candle,
    CoreMl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralModelStatus {
    pub model_id: String,
    pub purpose: NeuralModelPurpose,
    pub loaded: bool,
    pub model_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeuralModelPurpose {
    SemanticRouting,
    Embedding,
    Reranking,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralRankItem {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralRankRequest {
    pub query: String,
    pub items: Vec<NeuralRankItem>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralRankedItem {
    pub id: String,
    pub score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeuralRankResponse {
    pub owner: String,
    pub backend: NeuralBackend,
    pub ranked: Vec<NeuralRankedItem>,
}
