mod database;
mod frontend;
mod llms;
mod modalities;

pub use database::DatabaseEndpoints;
pub use frontend::FrontendInteractionEndpoints;
pub use llms::LlmEndpoints;
pub use modalities::{
    DesktopBroadcastRecord, ModalityEndpoints, ScreenFrameBroadcastRecord, ScreenshotRecord,
    VoiceTranscriptionRecord,
};
