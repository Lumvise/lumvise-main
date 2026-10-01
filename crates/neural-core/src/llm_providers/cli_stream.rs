use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmStreamEvent;
use crate::llm_providers::contract::LlmStreamEventSink;
use crate::process::StreamControl;
use serde_json::Value;

pub(crate) struct CliJsonTextStreamState {
    control: StreamControl,
    event_count: usize,
    cancelled: bool,
}

impl CliJsonTextStreamState {
    pub(crate) fn new(control: StreamControl) -> Self {
        Self {
            control,
            event_count: 0,
            cancelled: false,
        }
    }

    pub(crate) fn push_line(
        &mut self,
        provider: &str,
        line: &str,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        if self.cancelled {
            return Ok(());
        }
        let Some(text) = stream_json_text_delta(provider, line)? else {
            return Ok(());
        };
        self.event_count += 1;
        on_event(LlmStreamEvent::ContentDelta { text })?;
        self.cancelled = self.control.should_cancel_after(self.event_count);
        Ok(())
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled
    }
}

pub(crate) fn stream_json_text_delta(provider: &str, line: &str) -> Result<Option<String>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|source| NeuralError::Json {
        value: provider.to_string(),
        expected: "provider stream-json line".to_string(),
        source,
    })?;
    if let Some(message) = provider_error_message(&value) {
        return Err(NeuralError::ProviderFailed {
            provider_id: provider.to_string(),
            message,
        });
    }
    Ok(text_delta_from_value(&value))
}

fn provider_error_message(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("error") && value.get("error").is_none() {
        return None;
    }
    value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn text_delta_from_value(value: &Value) -> Option<String> {
    if ignored_stream_event(value) {
        return None;
    }
    direct_text_delta(value).or_else(|| content_array_text(value))
}

fn ignored_stream_event(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("init" | "result" | "tool_use" | "tool_result")
    )
}

fn direct_text_delta(value: &Value) -> Option<String> {
    [
        value.pointer("/delta/text"),
        value.get("delta"),
        value.get("content"),
        value.get("text"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .filter(|text| !text.is_empty())
    .map(str::to_string)
}

fn content_array_text(value: &Value) -> Option<String> {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|entry| entry.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
        .into_non_empty()
}

trait NonEmptyString {
    fn into_non_empty(self) -> Option<String>;
}

impl NonEmptyString for String {
    fn into_non_empty(self) -> Option<String> {
        if self.is_empty() { None } else { Some(self) }
    }
}
