//! Public token-range planning contract owned by Neural Core.
//! Callers may plan source ranges; tokenizer configuration and assets stay internal.

use lumvise_db_core::TextEmbeddingTask;
use serde::{Deserialize, Serialize};

/// Token budget and task for exact source-preserving chunk discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextChunkPlanRequest {
    pub text: String,
    pub task: TextEmbeddingTask,
    pub max_tokens: usize,
}

/// Original-source UTF-8 byte range whose budget includes task prefixes and special tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextChunkRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_count: usize,
}

/// Complete source coverage and the exact embedding profile used to plan it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextChunkPlan {
    pub engine_id: String,
    pub model: Option<String>,
    pub profile_fingerprint: String,
    pub ranges: Vec<TextChunkRange>,
}

/// Plans exact chunks without changing the embedding tokenizer or persisting source text.
///
/// Example: `planner.plan_text_chunks(&TextChunkPlanRequest { text: "body".into(),
/// task: TextEmbeddingTask::Document, max_tokens: 1024 })`.
pub trait TextChunkPlanner: Send + Sync {
    fn plan_text_chunks(&self, request: &TextChunkPlanRequest) -> crate::Result<TextChunkPlan>;

    /// Returns the exact profile identity without tokenizing or disclosing source text.
    /// Example: `planner.embedding_profile_fingerprint()?` before guarded inference.
    fn embedding_profile_fingerprint(&self) -> crate::Result<String>;
}
