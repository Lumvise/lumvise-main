//! Owns OpenAI-compatible tool-result text rendering. It unwraps only a plain
//! single-text result to avoid serializing the MCP envelope into another string.

use serde_json::Value;

pub(super) fn render_tool_result_content(result: &Value) -> String {
    single_text_content(result)
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string()))
}

fn single_text_content(result: &Value) -> Option<&str> {
    let result = result.as_object()?;
    if result.len() != 1 + usize::from(result.contains_key("isError"))
        || result
            .get("isError")
            .is_some_and(|is_error| is_error.as_bool() != Some(false))
    {
        return None;
    }
    let blocks = result.get("content")?.as_array()?;
    if blocks.len() != 1 {
        return None;
    }
    let block = blocks[0].as_object()?;
    if block.len() != 2 || block.get("type")?.as_str()? != "text" {
        return None;
    }
    block.get("text")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::render_tool_result_content;
    use serde_json::{Value, json};

    #[test]
    fn renders_exact_single_text_with_optional_false_error_flag() {
        let text = "quoted \"text\" — line one\nline two";
        for result in [
            json!({ "content": [{ "type": "text", "text": text }] }),
            json!({ "isError": false, "content": [{ "type": "text", "text": text }] }),
        ] {
            assert_eq!(render_tool_result_content(&result), text);
        }
    }

    #[test]
    fn preserves_json_for_every_richer_or_error_result() {
        let cases = [
            json!({ "content": [{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }] }),
            json!({ "content": [{ "type": "image", "data": "x" }] }),
            json!({ "content": [{ "type": "resource", "resource": { "uri": "x" } }] }),
            json!({ "content": [{ "type": "text", "text": "x" }], "isError": true }),
            json!({ "content": [{ "type": "text", "text": "x" }], "isError": null }),
            json!({ "content": [{ "type": "text", "text": "x" }], "isError": "false" }),
            json!({ "content": [{ "type": "text", "text": "x" }], "structuredContent": { "x": 1 } }),
            json!({ "content": [{ "type": "text", "text": "x" }], "_meta": { "x": 1 } }),
            json!({ "content": [{ "type": "text", "text": "x", "annotations": {} }] }),
            json!({ "content": [{ "type": "text", "text": "x" }], "extra": true }),
            json!({}),
            json!({ "content": [] }),
            json!({ "content": [{ "type": "text", "text": 7 }] }),
        ];
        assert_json_fallback(cases);
    }

    fn assert_json_fallback(cases: [Value; 13]) {
        for result in cases {
            assert_eq!(render_tool_result_content(&result), result.to_string());
        }
    }
}
