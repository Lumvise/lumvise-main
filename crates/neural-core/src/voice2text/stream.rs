use crate::Result;

pub type Voice2TextStreamEventSink<'a> = dyn FnMut(Voice2TextStreamEvent) -> Result<()> + 'a;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Voice2TextStreamEvent {
    TranscriptChunk {
        sequence: u64,
        text: String,
        is_final: bool,
    },
    Complete,
    Error {
        message: String,
    },
}
