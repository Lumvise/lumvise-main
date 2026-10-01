use crate::error::{NeuralError, Result};
use serde_json::Value;

pub fn non_empty_cli_text(provider_id: &str, stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(NeuralError::MalformedPayload {
            value: "empty stdout".to_string(),
            expected: format!("non-empty {provider_id} output"),
        });
    }
    Ok(parse_text_payload(trimmed).unwrap_or_else(|| trimmed.to_string()))
}

pub fn json_field_text(provider_id: &str, stdout: &str, field: &str) -> Result<String> {
    let value = json_stdout(provider_id, stdout)?;
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| NeuralError::MalformedPayload {
            value: value.to_string(),
            expected: format!("{provider_id} JSON with non-empty `{field}`"),
        })
}

pub fn parse_text_payload(text: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    text_from_value(&value)
}

fn json_stdout(provider_id: &str, stdout: &str) -> Result<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(NeuralError::MalformedPayload {
            value: "empty stdout".to_string(),
            expected: format!("{provider_id} JSON stdout"),
        });
    }
    let value: Value = serde_json::from_str(trimmed).map_err(|source| NeuralError::Json {
        value: trimmed.to_string(),
        expected: format!("{provider_id} JSON stdout"),
        source,
    })?;
    if let Some(message) = provider_error_message(&value) {
        return Err(NeuralError::ProviderFailed {
            provider_id: provider_id.to_string(),
            message,
        });
    }
    Ok(value)
}

fn provider_error_message(value: &Value) -> Option<String> {
    value
        .pointer("/error/message")
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn text_from_value(value: &Value) -> Option<String> {
    if let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(parts) = value.as_array() {
        return text_from_array(parts);
    }
    direct_text(value).or_else(|| nested_text(value))
}

fn text_from_array(parts: &[Value]) -> Option<String> {
    let text = parts
        .iter()
        .filter_map(text_from_value)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn direct_text(value: &Value) -> Option<String> {
    for key in [
        "response",
        "result",
        "final_response",
        "final_answer",
        "answer",
        "content",
        "message",
        "output_text",
        "text",
    ] {
        if let Some(text) = value.get(key).and_then(text_from_value) {
            return Some(text);
        }
    }
    None
}

fn nested_text(value: &Value) -> Option<String> {
    for key in ["messages", "items", "output"] {
        let Some(items) = value.get(key).and_then(Value::as_array) else {
            continue;
        };
        if let Some(text) = assistant_items_text(items) {
            return Some(text);
        }
    }
    None
}

fn assistant_items_text(items: &[Value]) -> Option<String> {
    for item in items.iter().rev() {
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("assistant");
        if role == "assistant" && !non_message_event(item) {
            return text_from_value(item);
        }
    }
    None
}

fn non_message_event(value: &Value) -> bool {
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return false;
    };
    !(kind.contains("message") || kind == "output_text")
}
