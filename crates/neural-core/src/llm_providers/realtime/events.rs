#![allow(
    dead_code,
    reason = "normalized realtime events are staged for provider consumers"
)]

use crate::error::{NeuralError, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RealtimeSessionEvent {
    SessionStarted {
        provider_session_id: String,
    },
    SessionUpdated {
        provider_session_id: Option<String>,
    },
    TranscriptDelta {
        item_id: Option<String>,
        text: String,
    },
    TranscriptFinal {
        item_id: Option<String>,
        text: String,
    },
    ResponseTextDelta {
        item_id: Option<String>,
        text: String,
    },
    ResponseTextFinal {
        item_id: Option<String>,
        text: String,
    },
    ResponseAudioDelta {
        item_id: Option<String>,
        audio: Vec<u8>,
        media_type: Option<String>,
    },
    ResponseAudioComplete {
        item_id: Option<String>,
    },
    Interrupted {
        reason: Option<String>,
    },
    Cancelled {
        reason: Option<String>,
    },
    Complete,
    Error {
        message: String,
    },
}

pub(crate) fn normalize_realtime_session_event(
    provider_id: &str,
    value: &Value,
) -> Result<Option<RealtimeSessionEvent>> {
    match value["type"].as_str() {
        Some("session.created") => Ok(Some(session_started(value)?)),
        Some("session.updated") => Ok(Some(session_updated(value))),
        Some(kind) if transcript_delta_kind(kind) => Ok(Some(transcript_delta(value)?)),
        Some(kind) if transcript_final_kind(kind) => Ok(Some(transcript_final(value)?)),
        Some(kind) if response_text_delta_kind(kind) => Ok(Some(response_text_delta(value)?)),
        Some(kind) if response_text_final_kind(kind) => Ok(Some(response_text_final(value)?)),
        Some("response.audio.delta" | "response.output_audio.delta") => {
            Ok(Some(response_audio_delta(value)?))
        }
        Some("response.audio.done" | "response.output_audio.done") => {
            Ok(Some(response_audio_complete(value)))
        }
        Some(kind) if interruption_kind(kind) => Ok(Some(interrupted(value))),
        Some("response.cancelled" | "response.canceled") => Ok(Some(cancelled(value))),
        Some("response.done" | "response.completed") => Ok(Some(response_done(provider_id, value))),
        Some("error") => Ok(Some(error_event(value))),
        _ => Ok(None),
    }
}

fn session_started(value: &Value) -> Result<RealtimeSessionEvent> {
    Ok(RealtimeSessionEvent::SessionStarted {
        provider_session_id: required_nested_string(value, &["session", "id"])?,
    })
}

fn session_updated(value: &Value) -> RealtimeSessionEvent {
    RealtimeSessionEvent::SessionUpdated {
        provider_session_id: nested_string(value, &["session", "id"]),
    }
}

fn transcript_delta(value: &Value) -> Result<RealtimeSessionEvent> {
    Ok(RealtimeSessionEvent::TranscriptDelta {
        item_id: event_item_id(value),
        text: required_event_text(value, &["delta", "transcript"])?,
    })
}

fn transcript_final(value: &Value) -> Result<RealtimeSessionEvent> {
    Ok(RealtimeSessionEvent::TranscriptFinal {
        item_id: event_item_id(value),
        text: required_event_text(value, &["transcript", "text"])?,
    })
}

fn response_text_delta(value: &Value) -> Result<RealtimeSessionEvent> {
    Ok(RealtimeSessionEvent::ResponseTextDelta {
        item_id: event_item_id(value),
        text: required_event_text(value, &["delta", "text", "transcript"])?,
    })
}

fn response_text_final(value: &Value) -> Result<RealtimeSessionEvent> {
    Ok(RealtimeSessionEvent::ResponseTextFinal {
        item_id: event_item_id(value),
        text: required_event_text(value, &["text", "transcript", "delta"])?,
    })
}

fn response_audio_delta(value: &Value) -> Result<RealtimeSessionEvent> {
    let encoded = required_event_text(value, &["delta", "audio"])?;
    Ok(RealtimeSessionEvent::ResponseAudioDelta {
        item_id: event_item_id(value),
        audio: decode_audio_delta(&encoded)?,
        media_type: optional_event_string(value, "media_type"),
    })
}

fn response_audio_complete(value: &Value) -> RealtimeSessionEvent {
    RealtimeSessionEvent::ResponseAudioComplete {
        item_id: event_item_id(value),
    }
}

fn response_done(provider_id: &str, value: &Value) -> RealtimeSessionEvent {
    match response_status(value) {
        Some("cancelled" | "canceled") => cancelled(value),
        Some("failed" | "incomplete") => provider_error(provider_id, value),
        _ => RealtimeSessionEvent::Complete,
    }
}

fn interrupted(value: &Value) -> RealtimeSessionEvent {
    RealtimeSessionEvent::Interrupted {
        reason: event_reason(value),
    }
}

fn cancelled(value: &Value) -> RealtimeSessionEvent {
    RealtimeSessionEvent::Cancelled {
        reason: event_reason(value),
    }
}

fn error_event(value: &Value) -> RealtimeSessionEvent {
    RealtimeSessionEvent::Error {
        message: event_message(value),
    }
}

fn provider_error(provider_id: &str, value: &Value) -> RealtimeSessionEvent {
    RealtimeSessionEvent::Error {
        message: format!(
            "{provider_id} realtime response did not complete: {}",
            event_message(value)
        ),
    }
}

fn transcript_delta_kind(kind: &str) -> bool {
    matches!(
        kind,
        "conversation.item.input_audio_transcription.delta" | "input_audio_transcription.delta"
    )
}

fn transcript_final_kind(kind: &str) -> bool {
    matches!(
        kind,
        "conversation.item.input_audio_transcription.completed"
            | "conversation.item.input_audio_transcription.final"
            | "input_audio_transcription.completed"
            | "input_audio_transcription.final"
    )
}

fn response_text_delta_kind(kind: &str) -> bool {
    matches!(
        kind,
        "response.text.delta"
            | "response.output_text.delta"
            | "response.audio_transcript.delta"
            | "response.output_audio_transcript.delta"
    )
}

fn response_text_final_kind(kind: &str) -> bool {
    matches!(
        kind,
        "response.text.done"
            | "response.output_text.done"
            | "response.audio_transcript.done"
            | "response.output_audio_transcript.done"
    )
}

fn interruption_kind(kind: &str) -> bool {
    matches!(
        kind,
        "conversation.interrupted"
            | "response.interrupted"
            | "conversation.item.truncated"
            | "input_audio_buffer.speech_started"
    )
}

fn required_event_text(value: &Value, keys: &[&str]) -> Result<String> {
    keys.iter()
        .find_map(|key| optional_event_string(value, key))
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| missing_event_text(value, keys))
}

fn required_nested_string(value: &Value, path: &[&str]) -> Result<String> {
    nested_string(value, path)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| missing_nested_string(value, path))
}

fn optional_event_string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_string)
}

fn nested_string(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn event_item_id(value: &Value) -> Option<String> {
    optional_event_string(value, "item_id")
        .or_else(|| nested_string(value, &["item", "id"]))
        .or_else(|| nested_string(value, &["response", "id"]))
}

fn event_reason(value: &Value) -> Option<String> {
    optional_event_string(value, "reason")
        .or_else(|| nested_string(value, &["response", "status_details", "reason"]))
        .or_else(|| nested_string(value, &["response", "status_details", "error", "message"]))
}

fn response_status(value: &Value) -> Option<&str> {
    value["response"]["status"].as_str()
}

fn event_message(value: &Value) -> String {
    nested_string(value, &["error", "message"])
        .or_else(|| optional_event_string(value, "message"))
        .or_else(|| event_reason(value))
        .unwrap_or_else(|| value.to_string())
}

fn decode_audio_delta(encoded: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|source| NeuralError::InvalidValue {
            value: encoded.to_string(),
            expected: format!("base64-encoded realtime response audio delta: {source}"),
        })
}

fn missing_event_text(value: &Value, keys: &[&str]) -> NeuralError {
    NeuralError::InvalidValue {
        value: value.to_string(),
        expected: format!("realtime event with non-empty text in one of {keys:?}"),
    }
}

fn missing_nested_string(value: &Value, path: &[&str]) -> NeuralError {
    NeuralError::InvalidValue {
        value: value.to_string(),
        expected: format!("realtime event with non-empty string at {path:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_session_created_and_updated_events() {
        let created = normalize(json!({
            "type": "session.created",
            "session": { "id": "sess_123" }
        }));
        let updated = normalize(json!({
            "type": "session.updated",
            "session": { "id": "sess_123" }
        }));

        assert_eq!(
            created,
            Some(RealtimeSessionEvent::SessionStarted {
                provider_session_id: "sess_123".to_string()
            })
        );
        assert_eq!(
            updated,
            Some(RealtimeSessionEvent::SessionUpdated {
                provider_session_id: Some("sess_123".to_string())
            })
        );
    }

    #[test]
    fn normalizes_partial_and_final_input_transcripts() {
        let delta = normalize(json!({
            "type": "conversation.item.input_audio_transcription.delta",
            "item_id": "item-a",
            "delta": "hel"
        }));
        let final_text = normalize(json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "item_id": "item-a",
            "transcript": "hello"
        }));

        assert_eq!(
            delta,
            Some(RealtimeSessionEvent::TranscriptDelta {
                item_id: Some("item-a".to_string()),
                text: "hel".to_string()
            })
        );
        assert_eq!(
            final_text,
            Some(RealtimeSessionEvent::TranscriptFinal {
                item_id: Some("item-a".to_string()),
                text: "hello".to_string()
            })
        );
    }

    #[test]
    fn normalizes_response_text_and_audio_events() {
        let text = normalize(json!({
            "type": "response.output_text.delta",
            "item_id": "item-b",
            "delta": "ok"
        }));
        let audio = normalize(json!({
            "type": "response.audio.delta",
            "item_id": "item-b",
            "delta": "AQID",
            "media_type": "audio/pcm;rate=24000"
        }));
        let audio_done = normalize(json!({
            "type": "response.audio.done",
            "item_id": "item-b"
        }));

        assert_eq!(
            text,
            Some(RealtimeSessionEvent::ResponseTextDelta {
                item_id: Some("item-b".to_string()),
                text: "ok".to_string()
            })
        );
        assert_eq!(
            audio,
            Some(RealtimeSessionEvent::ResponseAudioDelta {
                item_id: Some("item-b".to_string()),
                audio: vec![1, 2, 3],
                media_type: Some("audio/pcm;rate=24000".to_string())
            })
        );
        assert_eq!(
            audio_done,
            Some(RealtimeSessionEvent::ResponseAudioComplete {
                item_id: Some("item-b".to_string())
            })
        );
    }

    #[test]
    fn normalizes_control_and_terminal_events() {
        let interrupted = normalize(json!({
            "type": "input_audio_buffer.speech_started",
            "reason": "user_speech"
        }));
        let cancelled = normalize(json!({
            "type": "response.done",
            "response": {
                "status": "cancelled",
                "status_details": { "reason": "client_cancelled" }
            }
        }));
        let failed = normalize(json!({
            "type": "response.done",
            "response": {
                "status": "failed",
                "status_details": {
                    "error": { "message": "model overloaded" }
                }
            }
        }));
        let complete = normalize(json!({
            "type": "response.done",
            "response": { "status": "completed" }
        }));

        assert_eq!(
            interrupted,
            Some(RealtimeSessionEvent::Interrupted {
                reason: Some("user_speech".to_string())
            })
        );
        assert_eq!(
            cancelled,
            Some(RealtimeSessionEvent::Cancelled {
                reason: Some("client_cancelled".to_string())
            })
        );
        assert_eq!(complete, Some(RealtimeSessionEvent::Complete));
        assert_eq!(
            failed,
            Some(RealtimeSessionEvent::Error {
                message: "openai_realtime realtime response did not complete: model overloaded"
                    .to_string()
            })
        );
    }

    #[test]
    fn normalizes_error_and_ignores_unknown_events() {
        let error = normalize(json!({
            "type": "error",
            "error": { "message": "bad request" }
        }));
        let ignored = normalize(json!({
            "type": "rate_limits.updated"
        }));

        assert_eq!(
            error,
            Some(RealtimeSessionEvent::Error {
                message: "bad request".to_string()
            })
        );
        assert_eq!(ignored, None);
    }

    #[test]
    fn rejects_malformed_audio_delta_with_event_shape() {
        let error = normalize_realtime_session_event(
            "openai_realtime",
            &json!({ "type": "response.audio.delta", "delta": "not base64" }),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("not base64"));
        assert!(error.contains("base64-encoded realtime response audio delta"));
    }

    fn normalize(value: Value) -> Option<RealtimeSessionEvent> {
        normalize_realtime_session_event("openai_realtime", &value).unwrap()
    }
}
