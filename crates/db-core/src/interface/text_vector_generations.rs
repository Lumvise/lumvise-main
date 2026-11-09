//! Portable generation records; publication and recovery remain DB Core internals.

use serde::{Deserialize, Serialize};

/// Identity of one local-only section-vector publication attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElementTextVectorGenerationKey {
    pub project_root: String,
    pub semantic_element_id: String,
    pub generation_id: String,
}

/// Immutable publication inputs, containing no source text or infrastructure handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElementTextVectorGenerationSpec {
    pub key: ElementTextVectorGenerationKey,
    pub content_fingerprint: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub profile_fingerprint: String,
    pub dimensions: usize,
    pub chunk_count: u32,
    pub chunk_plan_fingerprint: String,
}

/// A terminal generation never reopens after replay or process restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementTextVectorGenerationState {
    Open,
    Committed,
    Aborted,
    Conflicted,
    Expired,
}

/// Durable acknowledgement used to reconcile publication with a rebuild cursor.
/// Aborting before begin returns Aborted with zero chunk and publication counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementTextVectorGenerationStatus {
    pub key: ElementTextVectorGenerationKey,
    pub state: ElementTextVectorGenerationState,
    pub next_chunk: u32,
    pub chunk_count: u32,
    pub published_count: usize,
}
