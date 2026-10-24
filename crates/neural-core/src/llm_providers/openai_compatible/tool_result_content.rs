//! Owns OpenAI-compatible tool receipts and rendered image evidence. Images enter
//! a following user message; ordinary tool results retain their existing text shape.

use serde_json::{Value, json};

pub(super) fn append_tool_result(
    messages: &mut Vec<Value>,
    id: &str,
    result: &Value,
    supports_images: bool,
) -> Option<Value> {
    let images: Vec<Value> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(image_part)
        .collect();
    if images.is_empty() {
        messages.push(
            json!({"role":"tool","tool_call_id":id,"content":render_tool_result_content(result)}),
        );
        return None;
    }
    let text = image_tool_receipt(result, supports_images);
    messages.push(json!({"role":"tool","tool_call_id":id,"content":text}));
    supports_images.then(|| image_evidence_message(images))
}

fn image_tool_receipt(result: &Value, supports_images: bool) -> String {
    let mut receipt = result.clone();
    receipt["content"] = Value::Array(
        result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|part| part["type"] != "image")
            .cloned()
            .collect(),
    );
    let mut text = render_tool_result_content(&receipt);
    if !supports_images {
        text.push_str(
            "\nThis provider does not accept image input. Do not claim visual inspection.",
        );
    }
    text
}

fn image_evidence_message(images: Vec<Value>) -> Value {
    let mut content = vec![
        json!({"type":"text","text":"Rendered visual evidence from the preceding tool. Inspect it before deciding whether the canvas needs corrections."}),
    ];
    content.extend(images);
    json!({"role":"user","content":content})
}

fn image_part(block: &Value) -> Option<Value> {
    if block["type"] != "image" {
        return None;
    }
    let mime = block["mimeType"]
        .as_str()
        .filter(|mime| mime.starts_with("image/"))?;
    let encoded = block["data"]
        .as_str()
        .filter(|encoded| !encoded.is_empty())?;
    Some(json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{encoded}")}}))
}

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
    use super::{append_tool_result, render_tool_result_content};
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
    fn image_capable_tools_deliver_pixels_in_a_user_image_part_after_the_tool_receipt() {
        let result = json!({"content":[{"type":"text","text":"revision 7"},{"type":"image","mimeType":"image/png","data":"pixels"}]});
        let mut messages = Vec::new();
        let visual = append_tool_result(&mut messages, "preview", &result, true).unwrap();
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "preview");
        assert_eq!(messages[0]["content"], "revision 7");
        assert_eq!(
            visual["content"][1]["image_url"]["url"],
            "data:image/png;base64,pixels"
        );
        assert!(!messages[0].to_string().contains("pixels"));
    }

    #[test]
    fn text_only_tools_omit_image_bytes_and_plain_tools_keep_their_message_shape() {
        let result = json!({"content":[{"type":"image","mimeType":"image/png","data":"pixels"}]});
        let mut messages = Vec::new();
        append_tool_result(&mut messages, "preview", &result, false);
        assert_eq!(messages.len(), 1);
        assert!(!messages[0].to_string().contains("pixels"));
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("does not accept image input")
        );
        append_tool_result(&mut messages, "plain", &json!({"value":3}), true);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"], "{\"value\":3}");
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
