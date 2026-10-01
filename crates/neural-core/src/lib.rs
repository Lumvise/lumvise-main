//! Neural Core owns Lumvise model, voice, vector, and semantic mapper runtime.

#[cfg(feature = "assistant-e2e")]
pub mod assistant_e2e;
pub mod config;
mod deferred_engine;
pub mod error;
pub mod llm_providers;
pub mod managed_models;
#[cfg(any(feature = "kokoros", feature = "whisper-rs"))]
pub(crate) mod model_assets;
pub mod process;
pub mod semantic_mappers;
pub mod speech;
pub mod speech_executor;
pub mod text2vector;
pub mod text2voice;
pub mod types;
pub mod voice2text;

pub use config::{EngineConfig, LlmProviderConfig, LlmProviderKind, NeuralCoreConfig, SpawnConfig};
pub use error::{NeuralError, Result};
pub use llm_providers::discovery::{
    LlmConfigurationRequirement, LlmModelDescriptor, LlmProviderAvailability, LlmProviderCandidate,
    LlmProviderCatalog, LlmProviderStatus, LlmProviderSync, LlmProviderSynchronizer,
};
pub use llm_providers::model_catalog::{
    LlmModelSource, ProviderModelCatalog, ProviderModelInventory, ProviderModelSources,
    ResolvedProviderModels,
};
pub use llm_providers::registry::LlmProviderRegistry;
pub use llm_providers::{
    CentralizedLlmProvider, CentralizedSpeechRecognizer, CentralizedSpeechSynthesizer,
    LlmMcpTransport, ScopedMcpTransport,
};
pub use managed_models::{
    HardwareFacts, HardwareProbe, HttpModelDownloader, ManagedModelCatalogEntry, ManagedModelKind,
    ManagedModelManager, ManagedModelSnapshot, ManagedModelState, ManagedModelStatus,
    ModelDownloader, ModelRuntimeAdapter, ModelSource, ModelSuitability, RuntimeHardwareProbe,
    builtin_catalog, lumvise_state_root, managed_models_root,
};
pub use semantic_mappers::service::SemanticMapperService;
pub use speech::{SpeechRecognizer, SpeechSynthesizer};
pub use speech_executor::{
    SPEECH_CHUNK_MAX_CHARS, SPEECH_CHUNK_MIN_CHARS, SpeechExecutionError, SpeechToTextExecutor,
    TextToSpeechExecutor, chunk_for_speech, sanitize_for_speech,
};
#[cfg(feature = "fastembed")]
pub use text2vector::{FastEmbedText2VectorConfig, FastEmbedText2VectorEngine};
pub use text2vector::{Text2VectorRuntimeConfig, service::Text2VectorService};
pub use text2voice::service::Text2VoiceService;
pub use voice2text::service::Voice2TextService;
