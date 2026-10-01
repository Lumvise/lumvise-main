use crate::types::EngineMetadata;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text2VectorResponse {
    pub vector: Vec<f32>,
    pub dimensions: usize,
    pub normalized: bool,
    pub metadata: EngineMetadata,
}
