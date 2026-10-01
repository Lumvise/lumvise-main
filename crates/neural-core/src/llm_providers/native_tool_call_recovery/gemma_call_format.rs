//! Gemma 4's native (non-JSON) tool-call wire format: `call:func_name
//! {key:value,key2:value2}`, with unquoted keys, `<|"|>`-delimited
//! strings, and otherwise-bare literals. See:
//! - <https://ai.google.dev/gemma/docs/core/prompt-formatting-gemma4>
//! - <https://docs.vllm.ai/en/latest/api/vllm/tool_parsers/gemma4_tool_parser/>
//!
//! Recovers exactly one call per turn, matching the shape actually
//! observed from Cerebras. Gemma's format allows multiple
//! `<|tool_call>...<tool_call|>`-wrapped calls concatenated in one
//! message; any content left over after the first parsed call is treated
//! as a structural parse failure rather than guessed at - callers must not
//! deliver such content to the user either.

use serde_json::{Map, Value};

use super::format::NativeToolCall;

const STRING_DELIMITER: &str = "<|\"|>";
const CALL_PREFIX: &str = "call:";

/// Detects and parses a Gemma-native tool call in already-trimmed,
/// wrapper-token-stripped `trimmed` content.
///
/// - `None`: `trimmed` does not have the `call:` prefix; not this format.
/// - `Some(Err(reason))`: the prefix is present but the content could not
///   be parsed structurally; the caller must not deliver `trimmed` to the
///   user.
/// - `Some(Ok(call))`: a call was recovered.
pub(super) fn parse(trimmed: &str) -> Option<Result<NativeToolCall, String>> {
    let rest = trimmed.strip_prefix(CALL_PREFIX)?;
    Some(parse_call(rest))
}

fn parse_call(rest: &str) -> Result<NativeToolCall, String> {
    let mut cursor = Cursor::new(rest);
    let name = cursor.read_identifier()?;
    cursor.expect_char('{')?;
    let arguments = cursor.read_object_body()?;
    cursor.skip_whitespace();
    if !cursor.at_end() {
        return Err(format!(
            "unexpected trailing content after tool call `{name}`"
        ));
    }
    Ok(NativeToolCall {
        name,
        arguments: Value::Object(arguments),
    })
}

struct Cursor {
    chars: Vec<char>,
    pos: usize,
}

impl Cursor {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
            self.pos += 1;
        }
    }

    fn matches_literal(&self, literal: &str) -> bool {
        let expected: Vec<char> = literal.chars().collect();
        self.pos + expected.len() <= self.chars.len()
            && self.chars[self.pos..self.pos + expected.len()] == expected[..]
    }

    fn consume_literal(&mut self, literal: &str) -> bool {
        if self.matches_literal(literal) {
            self.pos += literal.chars().count();
            true
        } else {
            false
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), String> {
        self.skip_whitespace();
        match self.advance() {
            Some(ch) if ch == expected => Ok(()),
            Some(ch) => Err(format!("expected `{expected}`, found `{ch}`")),
            None => Err(format!("expected `{expected}`, found end of input")),
        }
    }

    /// Reads an unquoted identifier: the tool name after `call:`, or an
    /// object key. Gemma's native format never quotes either.
    fn read_identifier(&mut self) -> Result<String, String> {
        self.skip_whitespace();
        let start = self.pos;
        while matches!(self.peek(), Some(ch) if ch.is_alphanumeric() || ch == '_') {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("expected an identifier".to_string());
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    fn read_object_body(&mut self) -> Result<Map<String, Value>, String> {
        let mut object = Map::new();
        self.skip_whitespace();
        if self.peek() == Some('}') {
            self.advance();
            return Ok(object);
        }
        loop {
            let key = self.read_identifier()?;
            self.expect_char(':')?;
            let value = self.read_value()?;
            object.insert(key, value);
            self.skip_whitespace();
            match self.advance() {
                Some(',') => {
                    self.skip_whitespace();
                }
                Some('}') => break,
                Some(ch) => {
                    return Err(format!(
                        "expected `,` or `}}` after object member, found `{ch}`"
                    ));
                }
                None => return Err("unterminated object; expected `}`".to_string()),
            }
        }
        Ok(object)
    }

    fn read_array_body(&mut self) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(']') {
            self.advance();
            return Ok(items);
        }
        loop {
            items.push(self.read_value()?);
            self.skip_whitespace();
            match self.advance() {
                Some(',') => {
                    self.skip_whitespace();
                }
                Some(']') => break,
                Some(ch) => {
                    return Err(format!(
                        "expected `,` or `]` after array element, found `{ch}`"
                    ));
                }
                None => return Err("unterminated array; expected `]`".to_string()),
            }
        }
        Ok(items)
    }

    fn read_value(&mut self) -> Result<Value, String> {
        self.skip_whitespace();
        match self.peek() {
            Some('{') => {
                self.advance();
                Ok(Value::Object(self.read_object_body()?))
            }
            Some('[') => {
                self.advance();
                Ok(Value::Array(self.read_array_body()?))
            }
            Some(_) if self.matches_literal(STRING_DELIMITER) => self.read_delimited_string(),
            Some(_) => self.read_bareword_value(),
            None => Err("expected a value, found end of input".to_string()),
        }
    }

    fn read_delimited_string(&mut self) -> Result<Value, String> {
        self.consume_literal(STRING_DELIMITER);
        let start = self.pos;
        while !self.matches_literal(STRING_DELIMITER) {
            if self.advance().is_none() {
                return Err("unterminated delimited string; expected closing `<|\"|>`".to_string());
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        self.consume_literal(STRING_DELIMITER);
        Ok(Value::String(text))
    }

    /// Reads an unquoted bareword value up to the next structural
    /// delimiter (`,`, `}`, `]`, or end of input) at this nesting level,
    /// then interprets it as `null`/`true`/`false`/a number/a plain
    /// string - Gemma's format never quotes bare literals, including
    /// strings that contain spaces (e.g. `App Core`).
    fn read_bareword_value(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while !matches!(self.peek(), Some(',') | Some('}') | Some(']') | None) {
            self.pos += 1;
        }
        let raw: String = self.chars[start..self.pos].iter().collect();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("expected a value".to_string());
        }
        Ok(bareword_literal(trimmed))
    }
}

fn bareword_literal(text: &str) -> Value {
    match text {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if let Ok(int) = text.parse::<i64>() {
        return Value::Number(int.into());
    }
    if let Ok(float) = text.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(float)
    {
        return Value::Number(number);
    }
    Value::String(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovers_the_call_observed_from_cerebras_for_a_canvas_diff() {
        let content = "call:builtin_assistant__canvas_apply_diff{patch:[{op:add,\
            path:/document/elementsById/mod-app-core,\
            value:{angle:0,backgroundColor:#e7f5ff,type:rectangle,width:200,x:400,y:100}},\
            {op:add,path:/document/elementsById/txt-app-core,\
            value:{originalText:App Core,text:App Core,type:text,opacity:100}}]}";
        let call = parse(content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.name, "builtin_assistant__canvas_apply_diff");
        let patch = call.arguments["patch"].as_array().expect("patch array");
        assert_eq!(patch.len(), 2);
        assert_eq!(patch[0]["op"], json!("add"));
        assert_eq!(
            patch[0]["path"],
            json!("/document/elementsById/mod-app-core")
        );
        assert_eq!(patch[0]["value"]["angle"], json!(0));
        assert_eq!(patch[0]["value"]["backgroundColor"], json!("#e7f5ff"));
        assert_eq!(patch[0]["value"]["type"], json!("rectangle"));
        assert_eq!(patch[0]["value"]["width"], json!(200));
        assert_eq!(patch[1]["value"]["originalText"], json!("App Core"));
        assert_eq!(patch[1]["value"]["opacity"], json!(100));
    }

    #[test]
    fn recovers_calls_using_the_documented_string_delimiter() {
        let content = r#"call:func_name{location:<|"|>Paris, France<|"|>,num:42}"#;
        let call = parse(content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.name, "func_name");
        assert_eq!(call.arguments["location"], json!("Paris, France"));
        assert_eq!(call.arguments["num"], json!(42));
    }

    #[test]
    fn a_malformed_call_is_reported_as_a_parse_failure_not_silently_dropped() {
        let unterminated = parse("call:builtin_assistant__canvas_apply_diff{patch:[");
        assert!(unterminated.expect("a call was attempted").is_err());

        let missing_open_brace = parse("call:builtin_assistant__assistant_respond");
        assert!(missing_open_brace.expect("a call was attempted").is_err());
    }

    #[test]
    fn trailing_content_after_a_closed_call_is_a_parse_failure() {
        let concatenated = parse("call:a{x:1}call:b{y:2}");
        assert!(concatenated.expect("a call was attempted").is_err());
    }

    #[test]
    fn nested_arrays_and_booleans_round_trip() {
        let content = "call:tool{flags:[true,false,null],nested:{deep:{value:1.5}}}";
        let call = parse(content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.arguments["flags"], json!([true, false, Value::Null]));
        assert_eq!(call.arguments["nested"]["deep"]["value"], json!(1.5));
    }
}
