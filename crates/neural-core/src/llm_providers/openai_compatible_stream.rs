use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmStreamEvent;
use crate::llm_providers::contract::LlmStreamEventSink;
use crate::process::StreamControl;
use serde_json::Value;

pub(crate) struct OpenAiCompatibleStreamState {
    buffer: String,
    control: StreamControl,
    event_count: usize,
    stopped: bool,
    provider_label: &'static str,
}

impl OpenAiCompatibleStreamState {
    pub(crate) fn new(control: StreamControl, provider_label: &'static str) -> Self {
        Self {
            buffer: String::new(),
            control,
            event_count: 0,
            stopped: false,
            provider_label,
        }
    }

    pub(crate) fn push_chunk(
        &mut self,
        chunk: &str,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.buffer.push_str(chunk);
        self.drain_sse_events(on_event)
    }

    pub(crate) fn finish(&mut self, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        if !self.stopped && !self.buffer.trim().is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.drain_event(&event, on_event)?;
        }
        Ok(())
    }

    fn drain_sse_events(&mut self, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        while let Some((index, delimiter_len)) = next_sse_delimiter(&self.buffer) {
            let event = self.buffer[..index].to_string();
            self.buffer.drain(..index + delimiter_len);
            self.drain_event(&event, on_event)?;
            if self.stopped {
                return Ok(());
            }
        }
        Ok(())
    }

    fn drain_event(&mut self, event: &str, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        for data in sse_data_lines(event) {
            self.drain_data(data, on_event)?;
            if self.stopped {
                return Ok(());
            }
        }
        Ok(())
    }

    fn drain_data(&mut self, data: &str, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        if data == "[DONE]" {
            self.emit(LlmStreamEvent::Complete, on_event)?;
            self.stopped = true;
            return Ok(());
        }
        if let Some(event) = delta_event(data, self.provider_label)? {
            self.emit(event, on_event)?;
        }
        Ok(())
    }

    fn emit(&mut self, event: LlmStreamEvent, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        let cancellable = matches!(
            event,
            LlmStreamEvent::ContentDelta { .. } | LlmStreamEvent::Error { .. }
        );
        on_event(event)?;
        self.event_count += 1;
        if cancellable && self.control.should_cancel_after(self.event_count) {
            on_event(LlmStreamEvent::Cancelled)?;
            self.stopped = true;
        }
        Ok(())
    }
}

fn delta_event(data: &str, provider_label: &str) -> Result<Option<LlmStreamEvent>> {
    let value: Value = serde_json::from_str(data).map_err(|source| NeuralError::Json {
        value: data.to_string(),
        expected: format!("{provider_label} SSE JSON data"),
        source,
    })?;
    if let Some(message) = provider_error(&value) {
        return Ok(Some(LlmStreamEvent::Error { message }));
    }
    if let Some(text) =
        optional_choice_content(&value, provider_label)?.filter(|text| !text.is_empty())
    {
        return Ok(Some(LlmStreamEvent::ContentDelta { text }));
    }
    Ok(None)
}

pub(crate) fn next_sse_delimiter(buffer: &str) -> Option<(usize, usize)> {
    let newline = buffer.find("\n\n").map(|index| (index, 2));
    let crlf = buffer.find("\r\n\r\n").map(|index| (index, 4));
    match (newline, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(position), None) | (None, Some(position)) => Some(position),
        (None, None) => None,
    }
}

pub(crate) fn sse_data_lines(event: &str) -> Vec<&str> {
    event
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        .filter(|line| !line.is_empty())
        .collect()
}

fn optional_choice_content(value: &Value, provider_label: &str) -> Result<Option<String>> {
    let Some(content) = choice_content_value(value) else {
        return Ok(None);
    };
    content_text(content)
        .map(Some)
        .ok_or_else(|| malformed_payload(value.clone(), &format!("{provider_label} content text")))
}

fn choice_content_value(value: &Value) -> Option<&Value> {
    value
        .get("choices")?
        .as_array()?
        .first()?
        .get("delta")?
        .get("content")
}

fn content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let text = content
        .as_array()?
        .iter()
        .filter_map(content_part_text)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn content_part_text(part: &Value) -> Option<String> {
    part.as_str()
        .or_else(|| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn provider_error(value: &Value) -> Option<String> {
    value
        .get("error")
        .and_then(|error| error.get("message").or(Some(error)))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn malformed_payload(value: Value, expected: &str) -> NeuralError {
    NeuralError::MalformedPayload {
        value: value.to_string(),
        expected: expected.to_string(),
    }
}
