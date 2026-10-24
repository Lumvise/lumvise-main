//! Encodes trusted plugin results once for regular and scoped MCP transports.
//! Native MCP image blocks stay images; ordinary domain objects remain JSON text.

use serde_json::{Value, json};

/// Preserve native MCP images and structured results; ordinary values become JSON text.
///
/// Example: `let result = lumvise_mcp_core::encode_mcp_tool_result(serde_json::json!({"revision": 4}), false);`
pub fn encode_mcp_tool_result(payload: Value, is_error: bool) -> Value {
    if !is_error && is_content_result(&payload) {
        let mut result = payload;
        result["isError"] = json!(false);
        return result;
    }
    json!({"content":[{"type":"text","text":payload.to_string()}],"isError":is_error})
}

fn is_content_result(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "content" | "structuredContent" | "isError"))
        || object.get("isError").is_some_and(|value| value != false)
    {
        return false;
    }
    object
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| !blocks.is_empty() && blocks.iter().all(is_native_content_block))
}

fn is_native_content_block(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("text") => block["text"].is_string(),
        Some("image") => {
            block["mimeType"]
                .as_str()
                .is_some_and(|mime| mime.starts_with("image/"))
                && block["data"]
                    .as_str()
                    .is_some_and(|encoded| !encoded.is_empty())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_image_and_pending_results_survive_without_json_stringification() {
        for block in [
            json!({"type":"image","mimeType":"image/png","data":"png"}),
            json!({"type":"text","text":"pending"}),
        ] {
            let payload = json!({"content":[block],"structuredContent":{"revision":4}});
            let result = encode_mcp_tool_result(payload.clone(), false);
            assert_eq!(result["content"], payload["content"]);
            assert_eq!(result["structuredContent"]["revision"], 4);
            assert_eq!(result["isError"], false);
        }
    }

    #[test]
    fn domain_objects_malformed_blocks_and_errors_keep_the_text_envelope() {
        for payload in [
            json!({"content":"document body"}),
            json!({"content":[{"type":"image","data":"x"}]}),
            json!({"content":[{"type":"text","text":"body"}],"document_id":"user-document"}),
        ] {
            assert_eq!(
                encode_mcp_tool_result(payload.clone(), false)["content"][0]["text"],
                payload.to_string()
            );
        }
        let payload = json!({"content":[{"type":"text","text":"failure"}]});
        assert_eq!(
            encode_mcp_tool_result(payload.clone(), true)["content"][0]["text"],
            payload.to_string()
        );
    }
}
