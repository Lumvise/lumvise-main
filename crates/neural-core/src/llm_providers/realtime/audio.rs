//! Persistent duplex audio boundary. Callers own capture/playback; this module
//! owns the provider connection, wire dialect, and scoped MCP tool dispatch.
//! Only the request, command, event, and `LlmProvider::run_audio_session` contract
//! are public. Socket polling and provider JSON remain internal.

use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmRequest;
use serde_json::Value;
use std::sync::mpsc::Receiver;

/// Conversation configuration; PCM input is mono signed 16-bit little-endian at 24 kHz.
#[derive(Debug, Clone)]
pub struct AudioSessionRequest {
    pub conversation: LlmRequest,
    pub voice: Option<String>,
}

/// Input to one live connection. `Close` ends it; silence does not.
#[derive(Debug, Clone)]
pub enum AudioSessionCommand {
    Pcm(Vec<u8>),
    Text(String),
    /// Local VAD ended an utterance; provider VAD still owns audio commitment.
    UserTurnEnded,
    Pause,
    Resume,
    Interrupt {
        item_id: Option<String>,
        played_ms: u64,
    },
    PlaybackStopped {
        item_id: String,
        played_ms: u64,
    },
    Close,
}

/// Provider-independent events. Audio is PCM16 mono, with an explicit sample rate.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioSessionEvent {
    Ready {
        provider_session_id: Option<String>,
    },
    InputStarted,
    InputStopped,
    Transcript {
        item_id: Option<String>,
        text: String,
        final_revision: bool,
    },
    ResponseStarted {
        response_id: String,
    },
    ResponseText {
        response_id: String,
        text: String,
        final_revision: bool,
    },
    Audio {
        response_id: String,
        item_id: Option<String>,
        pcm: Vec<u8>,
        sample_rate: u32,
    },
    ResponseFinished {
        response_id: String,
    },
    Interrupted,
    Closed,
}

pub type AudioSessionEventSink<'a> = dyn FnMut(AudioSessionEvent) -> Result<()> + 'a;
pub type AudioSessionInput = Receiver<AudioSessionCommand>;

pub(super) fn invalid_audio(value: impl ToString, expected: &str) -> NeuralError {
    NeuralError::InvalidValue {
        value: value.to_string(),
        expected: expected.into(),
    }
}

pub(super) fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_audio(value, &format!("non-empty {key}")))
}

pub(super) fn decode_pcm(value: &Value, key: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    let encoded = required_string(value, key)?;
    let pcm = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .map_err(|_| invalid_audio(&encoded, "base64 PCM16 audio"))?;
    if pcm.is_empty() || pcm.len() % 2 != 0 {
        return Err(invalid_audio(pcm.len(), "non-empty even PCM16 byte count"));
    }
    Ok(pcm)
}
