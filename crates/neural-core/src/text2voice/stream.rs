use serde::{Deserialize, Serialize};

use crate::Result;

pub type Text2VoiceStreamEventSink<'a> = dyn FnMut(Text2VoiceStreamEvent) -> Result<()> + 'a;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Text2VoiceStreamEvent {
    AudioChunk {
        sequence: u64,
        audio: Vec<u8>,
        media_type: String,
    },
    Complete,
    Error {
        message: String,
    },
}
