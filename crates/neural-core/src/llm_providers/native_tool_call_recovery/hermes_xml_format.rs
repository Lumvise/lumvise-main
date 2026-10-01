//! A second observed native tool-call serialization, in a Hermes/Qwen-
//! style XML-tag shape: `<tool_call>func_name<arg_key>key</arg_key>
//! <arg_value>value</arg_value>...</tool_call>`. Also observed leaking
//! from non-Gemma models (e.g. a `zai-glm-4.7` model) on the same
//! transport that leaks the Gemma `call:` format - dispatch to this
//! format is therefore keyed on the provider transport (see `dispatch`),
//! not on any particular model name.
//!
//! Argument values are JSON when the model emits structured data (arrays,
//! objects, numbers) and a bare string otherwise; each is parsed as JSON
//! first, falling back to a plain string. Dispatch on success; structural
//! parse failure - never delivered to the user verbatim - on anything
//! that does not fit the tag shape, including narration text mixed with
//! an embedded call tag or multiple concatenated call tags.

use serde_json::{Map, Value};

use super::format::NativeToolCall;

const XML_CALL_OPEN: &str = "<tool_call>";
const XML_CALL_CLOSE: &str = "</tool_call>";
const XML_ARG_KEY_OPEN: &str = "<arg_key>";
const XML_ARG_KEY_CLOSE: &str = "</arg_key>";
const XML_ARG_VALUE_OPEN: &str = "<arg_value>";
const XML_ARG_VALUE_CLOSE: &str = "</arg_value>";

/// Detects and parses an XML-tag native tool call in already-trimmed,
/// wrapper-token-stripped `trimmed` content.
///
/// - `None`: `trimmed` has no `<tool_call>` marker at all; not this
///   format.
/// - `Some(Err(reason))`: a `<tool_call>` marker is present but `trimmed`
///   is not a single well-formed call - either it's a strict-prefix match
///   that fails to parse structurally, or the marker appears elsewhere in
///   mixed/multi-call content, which is never a single recoverable call
///   and must not be guessed at.
/// - `Some(Ok(call))`: a call was recovered.
pub(super) fn parse(trimmed: &str) -> Option<Result<NativeToolCall, String>> {
    if let Some(rest) = trimmed.strip_prefix(XML_CALL_OPEN) {
        return Some(parse_xml_call(rest));
    }
    // `<tool_call>` is never legitimate spoken prose, unlike `call:` (see
    // `ordinary_text_is_not_detected_as_a_call`, which intentionally
    // allows that substring mid-sentence). Observed Cerebras output can
    // place narration text before the tag, or concatenate multiple call
    // blocks in one message; neither shape is a single clean call this
    // parser recovers, so treat the tag appearing anywhere as a detected-
    // but-unresolvable attempt rather than guessing which part is safe to
    // deliver as text.
    if trimmed.contains(XML_CALL_OPEN) {
        return Some(Err(
            "tool call tag embedded in mixed or multi-call content; cannot be safely recovered"
                .to_string(),
        ));
    }
    None
}

/// Parses the tag body: `rest` is the text after the opening `<tool_call>`
/// tag. Any tag mismatch, unterminated tag, or trailing content is a
/// structural parse failure - never guessed at.
fn parse_xml_call(rest: &str) -> Result<NativeToolCall, String> {
    let rest = rest
        .strip_suffix(XML_CALL_CLOSE)
        .ok_or_else(|| format!("missing closing `{XML_CALL_CLOSE}` tag"))?;
    let (name, mut cursor) = match rest.find(XML_ARG_KEY_OPEN) {
        Some(idx) => (rest[..idx].trim(), &rest[idx..]),
        None => (rest.trim(), ""),
    };
    if name.is_empty() {
        return Err("tool call name is empty".to_string());
    }
    let mut arguments = Map::new();
    while !cursor.is_empty() {
        let cursor_rest = cursor.strip_prefix(XML_ARG_KEY_OPEN).ok_or_else(|| {
            format!("expected `{XML_ARG_KEY_OPEN}`, found unexpected trailing content")
        })?;
        let key_end = cursor_rest
            .find(XML_ARG_KEY_CLOSE)
            .ok_or_else(|| format!("missing closing `{XML_ARG_KEY_CLOSE}`"))?;
        let key = cursor_rest[..key_end].trim().to_string();
        let after_key = &cursor_rest[key_end + XML_ARG_KEY_CLOSE.len()..];
        let after_value_open = after_key
            .strip_prefix(XML_ARG_VALUE_OPEN)
            .ok_or_else(|| format!("expected `{XML_ARG_VALUE_OPEN}` after arg key `{key}`"))?;
        let value_end = after_value_open
            .find(XML_ARG_VALUE_CLOSE)
            .ok_or_else(|| format!("missing closing `{XML_ARG_VALUE_CLOSE}` for arg `{key}`"))?;
        let raw_value = &after_value_open[..value_end];
        let value = serde_json::from_str(raw_value.trim())
            .unwrap_or_else(|_| Value::String(raw_value.to_string()));
        if arguments.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate arg key `{key}`"));
        }
        cursor = &after_value_open[value_end + XML_ARG_VALUE_CLOSE.len()..];
    }
    Ok(NativeToolCall {
        name: name.to_string(),
        arguments: Value::Object(arguments),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovers_the_xml_tag_call_format_observed_from_cerebras() {
        let content = "<tool_call>builtin_assistant__canvas_apply_diff\
            <arg_key>agent_message</arg_key><arg_value>Adding the services layer.</arg_value>\
            <arg_key>base_revision</arg_key><arg_value>5</arg_value>\
            <arg_key>patch</arg_key><arg_value>[{\"op\": \"add\", \"path\": \"/document/elementsById/x\", \"value\": {\"id\": \"x\"}}]</arg_value>\
            </tool_call>";
        let call = parse(content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.name, "builtin_assistant__canvas_apply_diff");
        assert_eq!(
            call.arguments["agent_message"],
            json!("Adding the services layer.")
        );
        assert_eq!(call.arguments["base_revision"], json!(5));
        let patch = call.arguments["patch"].as_array().expect("patch array");
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0]["path"], json!("/document/elementsById/x"));
    }

    #[test]
    fn xml_tag_call_with_no_arguments_recovers_an_empty_object() {
        let content = "<tool_call>builtin_assistant__assistant_history</tool_call>";
        let call = parse(content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.name, "builtin_assistant__assistant_history");
        assert_eq!(call.arguments, json!({}));
    }

    #[test]
    fn xml_tag_call_missing_closing_tag_is_a_parse_failure() {
        let result = parse(
            "<tool_call>builtin_assistant__canvas_apply_diff<arg_key>patch</arg_key><arg_value>[]</arg_value>",
        );
        assert!(result.expect("a call was attempted").is_err());
    }

    #[test]
    fn xml_tag_call_with_mismatched_arg_tags_is_a_parse_failure() {
        let content = "<tool_call>tool<arg_key>k</arg_key>oops<arg_value>v</arg_value></tool_call>";
        assert!(parse(content).expect("a call was attempted").is_err());
    }

    #[test]
    fn narration_text_followed_by_an_embedded_call_tag_is_a_parse_failure() {
        let content = "Right below App Core, I'll add the database layer.\
            <tool_call>builtin_assistant__canvas_apply_diff\
            <arg_key>base_revision</arg_key><arg_value>1</arg_value></tool_call>";
        let result = parse(content).expect("a call was attempted");
        assert!(
            result.is_err(),
            "mixed narration+call content must never be recovered as either plain text or a call"
        );
    }

    #[test]
    fn multiple_concatenated_call_tags_in_one_message_are_a_parse_failure() {
        let content = "<tool_call>builtin_assistant__assistant_respond\
            <arg_key>content</arg_key><arg_value>hi</arg_value></tool_call>\
            <tool_call>builtin_assistant__canvas_apply_diff\
            <arg_key>base_revision</arg_key><arg_value>1</arg_value></tool_call>";
        let result = parse(content).expect("a call was attempted");
        assert!(
            result.is_err(),
            "concatenated multi-call content is not a single recoverable call"
        );
    }
}
