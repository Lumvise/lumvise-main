pub mod request;
pub mod response;
pub mod runtime_config;
pub mod service;
pub mod spawned_engine;

#[cfg(feature = "fastembed")]
pub mod fastembed_engine;

#[cfg(feature = "fastembed")]
pub use fastembed_engine::{FastEmbedText2VectorConfig, FastEmbedText2VectorEngine};
pub use request::Text2VectorRequest;
pub use response::Text2VectorResponse;
pub use runtime_config::Text2VectorRuntimeConfig;
pub use service::Text2VectorService;
