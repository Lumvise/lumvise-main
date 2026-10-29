//! Passive blob and vector records shared by native persistence and the Rust/WASM gateway.
//! Callers use Contracts or the unchanged DB Core crate-root reexports; policy stays outside this module.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBlob {
    pub content_ref: String,
    pub artifact_id: String,
    pub media_type: String,
    pub content: Vec<u8>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactTextVector {
    pub engine_id: String,
    pub model: Option<String>,
    pub dimensions: usize,
    pub vector: Vec<f32>,
    pub normalized: bool,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredArtifactTextVector {
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub source_text: String,
    pub vector: ArtifactTextVector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredSemanticElementNameVector {
    pub semantic_element_id: String,
    pub project_root: String,
    pub source_text: String,
    pub vector: ArtifactTextVector,
}

#[cfg(test)]
#[path = "record_payloads/tests.rs"]
mod tests;
