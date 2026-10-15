//! Recognizes the Assistant's public acknowledgement of a finalized turn or closed session.
//! A model's `final` argument alone is never evidence that delivery succeeded.

use serde_json::Value;

pub(super) fn finalized_response(messages: &[Value]) -> Option<String> {
    let last = messages
        .last()
        .filter(|message| message["role"] == "tool")?;
    let result: Value = serde_json::from_str(last["content"].as_str()?).ok()?;
    acknowledged_response(&result)
}

fn acknowledged_response(result: &Value) -> Option<String> {
    let result = assistant_result(result)?;
    finalized_assistant_result(&result)
}

fn finalized_assistant_result(result: &Value) -> Option<String> {
    if result["isError"] == true || result.get("error").is_some() {
        return None;
    }
    if result["decision"] == "assistant_finished"
        && matches!(
            result.pointer("/state/phase").and_then(Value::as_str),
            Some("completed" | "failed")
        )
    {
        // Closing already stops playback; do not produce another response for the closed session.
        return Some(String::new());
    }
    if result["decision"] == "assistant_responded"
        && result.pointer("/state/app_response_finalized") == Some(&Value::Bool(true))
    {
        return result
            .pointer("/state/last_response")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    None
}

pub(super) fn assistant_result(result: &Value) -> Option<Value> {
    if result["isError"] == true || result.get("error").is_some() {
        return None;
    }
    if result["decision"].is_string() {
        return Some(result.clone());
    }
    if let Some(content) = result.get("structuredContent") {
        return assistant_result(content);
    }
    result["content"].as_array()?.iter().find_map(|block| {
        let value: Value = serde_json::from_str(block["text"].as_str()?).ok()?;
        assistant_result(&value)
    })
}
