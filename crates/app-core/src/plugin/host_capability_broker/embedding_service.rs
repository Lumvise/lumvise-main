//! A captured planner/vectorizer pair; model replacement swaps the pair once.
use super::*;

pub(crate) type SharedPluginVectorizer = Arc<Mutex<Option<PluginEmbeddingService>>>;

#[derive(Clone)]
pub(crate) struct PluginEmbeddingService {
    pub(in crate::plugin) vectorizer: Arc<dyn ArtifactTextVectorizer + Send + Sync>,
    pub(super) planner: Option<Arc<dyn lumvise_neural_core::text2vector::TextChunkPlanner>>,
}

impl PluginEmbeddingService {
    pub(crate) fn unplanned(vectorizer: Arc<dyn ArtifactTextVectorizer + Send + Sync>) -> Self {
        Self {
            vectorizer,
            planner: None,
        }
    }

    pub(crate) fn planned<T>(service: Arc<T>) -> Self
    where
        T: ArtifactTextVectorizer + lumvise_neural_core::text2vector::TextChunkPlanner + 'static,
    {
        Self {
            vectorizer: service.clone(),
            planner: Some(service),
        }
    }
}
