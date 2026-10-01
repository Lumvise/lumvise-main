use crate::Result;

pub trait VectorEmbedder: Send + Sync {
    fn embed_text(&self, text: &str) -> Result<Vec<f32>>;
}
