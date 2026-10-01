pub mod request;
pub mod response;
pub mod service;
pub mod spawned_engine;
pub mod stream;

#[cfg(feature = "whisper-rs")]
pub(crate) mod model_assets;
#[cfg(feature = "whisper-rs")]
pub mod whisper_rs_engine;

pub use request::Voice2TextRequest;
pub use response::Voice2TextResponse;
pub use service::Voice2TextService;
pub use stream::{Voice2TextStreamEvent, Voice2TextStreamEventSink};
#[cfg(feature = "whisper-rs")]
pub use whisper_rs_engine::{WhisperRsVoice2TextConfig, WhisperRsVoice2TextEngine};
