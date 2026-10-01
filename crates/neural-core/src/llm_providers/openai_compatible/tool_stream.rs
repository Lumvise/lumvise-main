//! Assembles one streamed chat-completion round before exposing complete tool arguments.
//! Framing belongs to the shared SSE reader; execution remains in the provider's tool loop.

use crate::error::{NeuralError, Result};
use crate::llm_providers::openai_compatible_stream::{next_sse_delimiter, sse_data_lines};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct ToolCompletionStream {
    buffer: String,
    data_events: usize,
    generation_id: Option<String>,
    provider_route: Option<String>,
    message: Map<String, Value>,
    calls: BTreeMap<u64, Value>,
    reasoning: BTreeMap<u64, Value>,
    response: Option<Value>,
}

impl ToolCompletionStream {
    // Counts distinguish a provider still generating from keep-alives or
    // incomplete SSE framing without exposing prompt, reasoning, or tool text.
    pub(super) fn diagnostic_snapshot(&self) -> Value {
        json!({
            "generation_id": self.generation_id,
            "provider_route": self.provider_route,
            "data_events": self.data_events,
            "buffered_bytes": self.buffer.len(),
            "content_bytes": self.message.get("content").and_then(Value::as_str).map_or(0, str::len),
            "reasoning_bytes": (["reasoning", "reasoning_content"].iter()
                .filter_map(|key| self.message.get(*key).and_then(Value::as_str)).map(str::len).sum::<usize>()),
            "reasoning_detail_bytes": self.reasoning.values().flat_map(|detail|
                ["text", "data", "summary"].into_iter().filter_map(|key| detail[key].as_str()))
                .map(str::len).sum::<usize>(),
            "tool_calls": self.calls.len(),
            "tool_argument_bytes": self.calls.values()
                .filter_map(|call| call["function"]["arguments"].as_str()).map(str::len).sum::<usize>()
        })
    }

    pub(super) fn push(&mut self, chunk: &str) -> Result<bool> {
        self.buffer.push_str(chunk);
        while let Some((offset, length)) = next_sse_delimiter(&self.buffer) {
            let event = self.buffer[..offset].to_owned();
            self.buffer.drain(..offset + length);
            for data in sse_data_lines(&event) {
                self.accept(data)?;
                if self.response.is_some() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn finish(self) -> Result<Value> {
        self.response
            .ok_or_else(|| malformed("stream EOF", "a completed model turn before EOF"))
    }

    fn accept(&mut self, data: &str) -> Result<()> {
        self.data_events += 1;
        if data == "[DONE]" {
            self.complete(Value::Null);
            return Ok(());
        }
        let frame: Value = serde_json::from_str(data).map_err(|source| NeuralError::Json {
            value: data.into(),
            expected: "OpenAI-compatible SSE JSON".into(),
            source,
        })?;
        if self.generation_id.is_none() {
            self.generation_id = frame["id"].as_str().map(str::to_owned);
        }
        if self.provider_route.is_none() {
            self.provider_route = frame["provider"].as_str().map(str::to_owned);
        }
        if let Some(error) = frame.get("error") {
            return Err(malformed(error, "successful provider stream frame"));
        }
        if let Some(choice) = frame["choices"]
            .as_array()
            .and_then(|choices| choices.first())
        {
            self.accept_choice(choice)?;
        }
        Ok(())
    }

    fn accept_choice(&mut self, choice: &Value) -> Result<()> {
        let delta = &choice["delta"];
        for field in ["content", "reasoning", "reasoning_content", "refusal"] {
            append_text(&mut self.message, field, &delta[field])?;
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            self.accept_call(call)?;
        }
        for detail in delta["reasoning_details"].as_array().into_iter().flatten() {
            self.accept_reasoning(detail)?;
        }
        if !choice["finish_reason"].is_null() {
            self.complete(choice["finish_reason"].clone());
        }
        Ok(())
    }

    fn accept_call(&mut self, delta: &Value) -> Result<()> {
        let index = delta["index"]
            .as_u64()
            .ok_or_else(|| malformed(delta, "tool delta index"))?;
        let call = self
            .calls
            .entry(index)
            .or_insert_with(|| json!({"type":"function", "function":{}}));
        let fields = call
            .as_object_mut()
            .ok_or_else(|| malformed("tool", "tool object"))?;
        append_text(fields, "id", &delta["id"])?;
        let function = fields
            .get_mut("function")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| malformed("function", "tool function object"))?;
        append_text(function, "name", &delta["function"]["name"])?;
        append_text(function, "arguments", &delta["function"]["arguments"])
    }

    fn accept_reasoning(&mut self, delta: &Value) -> Result<()> {
        let index = delta["index"].as_u64().unwrap_or(0);
        let detail = self.reasoning.entry(index).or_insert_with(|| json!({}));
        let fields = detail
            .as_object_mut()
            .ok_or_else(|| malformed("reasoning", "reasoning object"))?;
        let updates = delta
            .as_object()
            .ok_or_else(|| malformed(delta, "reasoning detail object"))?;
        for (key, value) in updates {
            if matches!(key.as_str(), "text" | "data" | "summary") {
                append_text(fields, key, value)?;
            } else {
                fields.insert(key.clone(), value.clone());
            }
        }
        Ok(())
    }

    fn complete(&mut self, finish_reason: Value) {
        self.message.insert("role".into(), json!("assistant"));
        self.message.entry("content").or_insert(Value::Null);
        if !self.calls.is_empty() {
            self.message.insert(
                "tool_calls".into(),
                json!(
                    std::mem::take(&mut self.calls)
                        .into_values()
                        .collect::<Vec<_>>()
                ),
            );
        }
        if !self.reasoning.is_empty() {
            self.message.insert(
                "reasoning_details".into(),
                json!(
                    std::mem::take(&mut self.reasoning)
                        .into_values()
                        .collect::<Vec<_>>()
                ),
            );
        }
        self.response = Some(
            json!({"choices":[{"message":std::mem::take(&mut self.message),"finish_reason":finish_reason}]}),
        );
    }
}

fn append_text(fields: &mut Map<String, Value>, key: &str, delta: &Value) -> Result<()> {
    if delta.is_null() {
        return Ok(());
    }
    let text = delta
        .as_str()
        .ok_or_else(|| malformed(delta, "string or null stream delta"))?;
    let mut accumulated = fields
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    accumulated.push_str(text);
    fields.insert(key.into(), json!(accumulated));
    Ok(())
}

fn malformed(value: impl std::fmt::Display, expected: &str) -> NeuralError {
    NeuralError::MalformedPayload {
        value: value.to_string(),
        expected: expected.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_stream_diagnostics_separate_heartbeats_framing_and_generated_text() {
        let mut stream = ToolCompletionStream::default();
        stream.push(": OPENROUTER PROCESSING\n\n").unwrap();
        assert_eq!(stream.diagnostic_snapshot()["data_events"], 0);
        let frame = json!({"choices":[{"delta":{
            "content":"hello", "reasoning":"private thought",
            "tool_calls":[{"index":0,"function":{"name":"read","arguments":"{\"id\":"}}]
        }}]});
        let chunk = format!("data: {frame}\n\n");
        stream.push(&chunk[..7]).unwrap();
        assert_eq!(stream.diagnostic_snapshot()["buffered_bytes"], 7);
        stream.push(&chunk[7..]).unwrap();
        let counts = stream.diagnostic_snapshot();
        assert_eq!(counts["data_events"], 1);
        assert_eq!(counts["buffered_bytes"], 0);
        assert_eq!(counts["content_bytes"], 5);
        assert_eq!(counts["reasoning_bytes"], 15);
        assert_eq!(counts["tool_calls"], 1);
        assert_eq!(counts["tool_argument_bytes"], 6);
        assert!(!counts.to_string().contains("private thought"));
        assert!(!counts.to_string().contains("hello"));
    }
}
