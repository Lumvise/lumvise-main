pub mod request;
pub mod response;
pub mod service;
pub mod spawned_engine;
pub mod speech_text;
pub mod stream;

#[cfg(feature = "kokoros")]
mod kokoros_assets;

#[cfg(feature = "kokoros")]
pub mod kokoros_engine;

#[cfg(feature = "kokoros")]
pub use kokoros_engine::{KokorosText2VoiceConfig, KokorosText2VoiceEngine};
pub use request::Text2VoiceRequest;
pub use response::Text2VoiceResponse;
pub use service::Text2VoiceService;
pub use stream::{Text2VoiceStreamEvent, Text2VoiceStreamEventSink};
