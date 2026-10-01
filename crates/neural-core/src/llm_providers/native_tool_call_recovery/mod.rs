//! Centralized, per-provider dispatch for native (non-`tool_calls`)
//! tool-call leaks.
//!
//! Some LLM providers/models emit a tool call in their own native chat-
//! template wire format instead of a structured OpenAI `message.
//! tool_calls` entry - the raw native-format text then lands in
//! `message.content` instead. Every provider transport that talks to such
//! a model calls through this module exactly once, at the point where it
//! has confirmed there is no structured tool call in the provider's own
//! response shape, so a leaked native-format call is either recovered
//! (and dispatched through the tool catalog, identically to a real
//! structured tool call) or rejected - uniformly, regardless of which
//! provider or model produced `content`. A detected-but-unresolvable call
//! is always an error: `content` must never be delivered to the user in
//! that case.
//!
//! Which native formats are worth scanning for is provider-specific in
//! principle (see `dispatch`), even though today every known provider
//! kind maps to the same two known formats - see `dispatch`'s module doc
//! for why that is currently the correct, evidence-backed choice rather
//! than an unexercised abstraction.
//!
//! Known leaked formats live in their own modules: `gemma_call_format`
//! (Gemma 4's `call:name{...}` wire format) and `hermes_xml_format`
//! (Hermes/Qwen-style `<tool_call>name<arg_key>...` XML format, also
//! observed from non-Gemma models on the same transport). Adding a new
//! observed format means adding an `LlmCallFormat` variant (see
//! `format.rs`) and wiring it into `dispatch::applicable_formats` - the
//! compiler forces every match to account for it.

mod dispatch;
mod format;
mod gemma_call_format;
mod hermes_xml_format;
#[cfg(test)]
mod regressions;

use serde_json::Value;

use crate::config::LlmProviderKind;
use crate::error::NeuralError;
use crate::llm_providers::tool_invocation::{McpToolCatalog, tool_outcome_value};

use format::NativeToolCall;

const TEMPLATE_ARTIFACT_PREFIX: &str = "None";

/// Outcome of checking whether model `content` is really final text, or a
/// tool-call attempt in a provider's native serialization that it failed
/// to translate into its own structured tool-call response.
pub(crate) enum TextRecovery {
    FinalText(String),
    Recovered(RecoveredCall),
}

/// A native tool-call attempt that was recovered and dispatched.
pub(crate) struct RecoveredCall {
    pub(crate) name: String,
    pub(crate) arguments: Value,
    pub(crate) result: Value,
}

/// Every LLM provider's MCP tool loop calls this exactly once, at the
/// point where it has confirmed there is no structured tool call in the
/// provider's own native response shape - so a hallucinated/untranslated
/// call is recovered (and dispatched through `tools`, identically to a
/// real structured tool call) or rejected uniformly. `kind`/`model` select
/// which native formats are even scanned for (see `dispatch`).
pub(crate) fn recover_or_confirm_final_text(
    kind: LlmProviderKind,
    model: &str,
    content: String,
    tools: &McpToolCatalog,
) -> crate::error::Result<TextRecovery> {
    let call = match detect(kind, model, &content) {
        None => return Ok(TextRecovery::FinalText(content)),
        Some(Ok(call)) => call,
        Some(Err(reason)) => return Err(malformed_native_call(&content, &reason)),
    };
    if !tools.tools().iter().any(|tool| tool.name == call.name) {
        return Err(malformed_native_call(
            &content,
            &format!("unknown MCP tool `{}`", call.name),
        ));
    }
    let result = tool_outcome_value(tools.invoker().invoke(&call.name, call.arguments.clone()))?;
    Ok(TextRecovery::Recovered(RecoveredCall {
        name: call.name,
        arguments: call.arguments,
        result,
    }))
}

/// Used only by a provider's exhausted-tool-rounds fallback response,
/// where no further round exists to dispatch a recovered call against.
/// Any detected call attempt in `content` - parseable or not - must not
/// reach the user, so it is rejected the same way an unparseable call
/// already is; the caller surfaces this as a retryable turn failure
/// instead.
pub(crate) fn reject_undispatchable_call_attempt(
    kind: LlmProviderKind,
    model: &str,
    content: String,
) -> crate::error::Result<String> {
    match detect(kind, model, &content) {
        None => Ok(content),
        Some(Ok(call)) => Err(malformed_native_call(
            &content,
            &format!(
                "tool call `{}` attempted with no tool round left to dispatch it",
                call.name
            ),
        )),
        Some(Err(reason)) => Err(malformed_native_call(&content, &reason)),
    }
}

/// Detects and parses a leaked native tool call in `content`, trying only
/// the formats `dispatch::applicable_formats` says this `kind`/`model` is
/// known to emit.
///
/// - `None`: no applicable format recognized a call attempt in `content`;
///   the caller should treat it as an ordinary response.
/// - `Some(Err(reason))`: a call was clearly attempted but could not be
///   parsed structurally; the caller must not deliver `content` to the
///   user.
/// - `Some(Ok(call))`: a call was recovered; dispatch `call.name` with
///   `call.arguments` the same way a structured `tool_calls` entry would
///   be.
fn detect(
    kind: LlmProviderKind,
    model: &str,
    content: &str,
) -> Option<Result<NativeToolCall, String>> {
    let trimmed = strip_wrapper_tokens(content.trim());
    for format in dispatch::applicable_formats(kind, model) {
        if let Some(result) = format.parse(trimmed) {
            return Some(result);
        }
    }
    None
}

fn malformed_native_call(content: &str, reason: &str) -> NeuralError {
    NeuralError::MalformedPayload {
        value: content.to_string(),
        expected: format!("a well-formed native tool call or plain text response; {reason}"),
    }
}

fn strip_wrapper_tokens(content: &str) -> &str {
    let content = content.trim();
    // Cerebras/vLLM chat-template artifact: a literal `None` sometimes
    // leaks immediately before the XML-tag tool-call format.
    let content = match content.strip_prefix(TEMPLATE_ARTIFACT_PREFIX) {
        Some(rest) if rest.starts_with("<tool_call>") => rest,
        _ => content,
    };
    content
        .trim_start_matches("<|tool_call>")
        .trim_end_matches("<tool_call|>")
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CEREBRAS: LlmProviderKind = LlmProviderKind::Cerebras;

    #[test]
    fn ordinary_text_is_not_detected_as_a_call() {
        assert!(
            detect(
                CEREBRAS,
                "any-model",
                "Sure, here is the answer you asked for."
            )
            .is_none()
        );
        assert!(
            detect(
                CEREBRAS,
                "any-model",
                "The function call: this is not one either"
            )
            .is_none()
        );
    }

    #[test]
    fn strips_the_optional_tool_call_wrapper_tokens() {
        let content =
            "<|tool_call>call:builtin_assistant__assistant_respond{content:hi}<tool_call|>";
        let call = detect(CEREBRAS, "any-model", content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.name, "builtin_assistant__assistant_respond");
        assert_eq!(call.arguments["content"], json!("hi"));
    }

    #[test]
    fn strips_the_leaked_none_template_artifact_before_an_xml_tag_call() {
        let content = "None<tool_call>builtin_assistant__assistant_respond<arg_key>content</arg_key><arg_value>hi</arg_value></tool_call>";
        let call = detect(CEREBRAS, "any-model", content)
            .expect("a call was attempted")
            .expect("the call parses");
        assert_eq!(call.name, "builtin_assistant__assistant_respond");
        assert_eq!(call.arguments["content"], json!("hi"));
    }

    #[test]
    fn a_leaked_none_prefix_without_a_call_tag_is_ordinary_text() {
        assert!(detect(CEREBRAS, "any-model", "None of the above applies here.").is_none());
    }

    #[test]
    fn xml_tag_call_with_a_garbled_value_still_parses_as_a_call_attempt() {
        // A stray quote in an unparseable-as-JSON value falls back to a
        // raw string rather than causing a silent leak to the user:
        // `detect` still recognizes this as a call attempt (Some(Ok(..))),
        // so `recover_or_confirm_final_text` never treats it as ordinary
        // text.
        let content = "None<tool_call>builtin_assistant__canvas_apply_diff\
            <arg_key>base_revision</arg_key><arg_value>5\"</arg_value></tool_call>";
        let call = detect(CEREBRAS, "any-model", content)
            .expect("a call was attempted")
            .expect("the call parses despite the garbled value");
        assert_eq!(call.name, "builtin_assistant__canvas_apply_diff");
        assert_eq!(call.arguments["base_revision"], json!("5\""));
    }

    #[test]
    fn mixed_call_tag_content_never_reaches_recover_or_confirm_final_text_as_final_text() {
        let content = "Right below App Core, I'll add the database layer.\
            <tool_call>builtin_assistant__canvas_apply_diff\
            <arg_key>base_revision</arg_key><arg_value>1</arg_value></tool_call>";
        // `detect` alone is enough to prove `recover_or_confirm_final_text`
        // cannot fall through to `TextRecovery::FinalText(content)`: that
        // arm only runs when `detect` returns `None`.
        assert!(detect(CEREBRAS, "any-model", content).is_some());
    }

    #[test]
    fn reject_undispatchable_allows_ordinary_text() {
        let result = reject_undispatchable_call_attempt(
            CEREBRAS,
            "any-model",
            "Sure, here is the answer you asked for.".to_string(),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Sure, here is the answer you asked for.");
    }

    #[test]
    fn reject_undispatchable_rejects_a_parseable_call() {
        let result = reject_undispatchable_call_attempt(
            CEREBRAS,
            "any-model",
            "call:builtin_assistant__assistant_respond{content:hi}".to_string(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no tool round left"), "error: {err}");
    }

    #[test]
    fn reject_undispatchable_rejects_unparseable_malformed_call() {
        let result = reject_undispatchable_call_attempt(
            CEREBRAS,
            "any-model",
            "call:builtin_assistant__canvas_apply_diff{patch:[".to_string(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn reject_undispatchable_rejects_garbled_call_with_escaped_quotes() {
        let result = reject_undispatchable_call_attempt(
            CEREBRAS,
            "any-model",
            r#"call:builtin_assistant__canvas_apply_diff{base_revision:1,patch:[{op:remove\",\"path:/document/elementOrder/0\"}]}"#.to_string(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn every_known_provider_kind_recovers_the_same_leaked_gemma_call() {
        let content = "call:builtin_assistant__assistant_respond{content:hi}";
        for kind in [
            LlmProviderKind::Cerebras,
            LlmProviderKind::Claude,
            LlmProviderKind::Codex,
            LlmProviderKind::Gemini,
            LlmProviderKind::OpenAiRealtime,
            LlmProviderKind::OpenRouter,
            LlmProviderKind::Zai,
            LlmProviderKind::Local,
        ] {
            assert!(
                matches!(detect(kind, "any-model", content), Some(Ok(_))),
                "kind {kind:?} should recover the call - every known kind's format table \
                 includes GemmaCallPrefix (F-009: every direct-API provider must recover a \
                 leaked native call, not just Cerebras/OpenRouter/Zai)"
            );
        }
    }

    #[test]
    fn claude_also_rejects_native_looking_text_as_an_undispatchable_call() {
        let content = "<tool_call>foo</tool_call>".to_string();
        let claude = reject_undispatchable_call_attempt(
            LlmProviderKind::Claude,
            "claude-opus-5",
            content.clone(),
        );
        assert!(
            claude.is_err(),
            "claude's format table includes HermesXmlTag, matching cerebras below"
        );

        let cerebras = reject_undispatchable_call_attempt(CEREBRAS, "any-model", content);
        assert!(
            cerebras.is_err(),
            "cerebras recognizes the tag and must reject an undispatchable attempt"
        );
    }

    #[test]
    fn model_argument_does_not_currently_narrow_selection_within_a_known_kind() {
        let content = "call:builtin_assistant__assistant_respond{content:hi}";
        let gemma_named = detect(CEREBRAS, "gemma-4-31b", content);
        let non_gemma_named = detect(CEREBRAS, "zai-glm-4.7", content);
        assert!(
            matches!(gemma_named, Some(Ok(_))) && matches!(non_gemma_named, Some(Ok(_))),
            "documents today's intentionally coarse per-kind (not per-model) dispatch"
        );
    }
}
