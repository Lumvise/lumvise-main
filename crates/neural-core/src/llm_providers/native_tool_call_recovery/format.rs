//! The closed set of native (non-`tool_calls`) chat-template formats a
//! provider might leak into `message.content` instead of translating into
//! the OpenAI structured `tool_calls` field. Adding a new observed format
//! means adding a variant here and a `mod` for it - the compiler then
//! forces every exhaustive match (including the dispatch table in
//! `dispatch.rs`) to account for it.

use serde_json::Value;

/// A tool call recovered from a provider's native (non-structured)
/// serialization.
pub(super) struct NativeToolCall {
    pub(super) name: String,
    pub(super) arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LlmCallFormat {
    /// Gemma-native `call:func_name{key:value,key2:value2}` wire format.
    /// See `gemma_call_format` for the format documentation.
    GemmaCallPrefix,
    /// Hermes/Qwen-style `<tool_call>name<arg_key>k</arg_key>
    /// <arg_value>v</arg_value>...</tool_call>` wire format. See
    /// `hermes_xml_format` for the format documentation.
    HermesXmlTag,
}

impl LlmCallFormat {
    /// Detects and parses this one format against already-trimmed,
    /// wrapper-token-stripped content.
    ///
    /// - `None`: `trimmed` has no marker for this format; the caller
    ///   should try the next applicable format, or treat content as
    ///   ordinary text if none match.
    /// - `Some(Err(reason))`: a marker for this format was present, but
    ///   the content is not a single well-formed call (fail closed -
    ///   never guessed at).
    /// - `Some(Ok(call))`: exactly one call was recovered.
    pub(super) fn parse(self, trimmed: &str) -> Option<Result<NativeToolCall, String>> {
        match self {
            Self::GemmaCallPrefix => super::gemma_call_format::parse(trimmed),
            Self::HermesXmlTag => super::hermes_xml_format::parse(trimmed),
        }
    }
}
