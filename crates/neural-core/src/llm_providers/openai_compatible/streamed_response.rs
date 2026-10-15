//! Owns incremental plain-text delivery for opted-in Assistant turns.
//! The provider tool loop feeds text and round boundaries here. Scoped tools
//! still own session state, authorization and speech; tool arguments never stream.

use super::turn_completion;
use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmRequest;
use crate::llm_providers::command_runner::selected_model;
use crate::llm_providers::native_tool_call_recovery::reject_undispatchable_call_attempt;
use crate::llm_providers::tool_invocation::{McpToolCatalog, tool_outcome_value};
use crate::process::StreamControl;
use crate::speech_executor::SPEECH_CHUNK_MAX_CHARS;
use serde_json::{Value, json};

pub(super) struct StreamedToolResponse<'a> {
    config: &'a LlmProviderConfig,
    tools: &'a McpToolCatalog,
    tool_name: &'a str,
    model: String,
    round_text: String,
    consumed: usize,
    suppressed: bool,
    delivered: String,
    control: &'a StreamControl,
}

impl<'a> StreamedToolResponse<'a> {
    pub(super) fn new(
        config: &'a LlmProviderConfig,
        request: &'a LlmRequest,
        tools: &'a McpToolCatalog,
        control: &'a StreamControl,
    ) -> Result<Option<Self>> {
        let Some(tool_name) = request.options.streamed_response_tool.as_deref() else {
            return Ok(None);
        };
        require_response_tool(config, tools, tool_name)?;
        Ok(Some(Self {
            config,
            tools,
            tool_name,
            control,
            model: selected_model(config, request),
            round_text: String::new(),
            consumed: 0,
            suppressed: false,
            delivered: String::new(),
        }))
    }

    pub(super) fn tool_name(&self) -> &str {
        self.tool_name
    }

    pub(super) fn instructions(&self) -> Value {
        json!({"role":"system","content":format!(
            "Response delivery for this turn: your ordinary assistant content is streamed directly to the user as speech. This replaces all instructions to call assistant.respond or {}. Write only plain spoken prose in content, with short complete sentences. Never repeat narration as a tool call. Other tools remain available: briefly explain the next piece in content, then call the appropriate tool with complete arguments. Keep reasoning and tool serialization out of content. After a final spoken answer, stop; the app closes delivery automatically.", self.tool_name)})
    }

    pub(super) fn push(&mut self, text: &str) -> Result<()> {
        self.round_text.push_str(text);
        // Reuse the provider's leak detection before a serialized tool can reach speech.
        self.suppressed |= reject_undispatchable_call_attempt(
            self.config.kind,
            &self.model,
            self.round_text.clone(),
        )
        .is_err();
        if self.suppressed {
            return Ok(());
        }
        while let Some(end) = speech_boundary(&self.round_text[self.consumed..]) {
            let end = self.consumed + end;
            let sentence = self.round_text[self.consumed..end].trim().to_owned();
            self.deliver(&sentence, false)?;
            self.consumed = end;
        }
        Ok(())
    }

    pub(super) fn finish_round(&mut self, final_response: bool) -> Result<String> {
        let tail = self.round_text[self.consumed..].trim().to_owned();
        if !self.suppressed && (!tail.is_empty() || final_response) {
            self.deliver(&tail, final_response)?;
        }
        self.round_text.clear();
        self.consumed = 0;
        self.suppressed = false;
        Ok(self.delivered.clone())
    }

    fn deliver(&mut self, content: &str, final_response: bool) -> Result<()> {
        super::check_tool_cancellation(self.config, self.control)?;
        let result = tool_outcome_value(self.tools.invoker().invoke(
            self.tool_name,
            json!({"content":content,"final":final_response}),
        ))?;
        self.delivered = delivery_text(self.config, &result, final_response)?;
        tracing::info!(event = "llm_speech_segment_delivered", provider_id = %self.config.provider_id,
            characters = content.chars().count(), final_response);
        Ok(())
    }
}

fn require_response_tool(
    config: &LlmProviderConfig,
    tools: &McpToolCatalog,
    name: &str,
) -> Result<()> {
    if tools.tools().iter().any(|tool| tool.name == name) {
        return Ok(());
    }
    Err(failed(
        config,
        format!("streamed response tool `{name}`; expected a discovered scoped Assistant tool"),
    ))
}

fn delivery_text(
    config: &LlmProviderConfig,
    result: &Value,
    final_response: bool,
) -> Result<String> {
    let reply = turn_completion::assistant_result(result).ok_or_else(|| {
        failed(
            config,
            format!("speech result `{result}`; expected Assistant delivery acknowledgement"),
        )
    })?;
    if reply["decision"] == "cancelled" {
        return Err(NeuralError::ProcessCancelled {
            command: config.provider_id.clone(),
        });
    }
    require_delivery_acknowledgement(config, &reply, final_response)?;
    reply["state"]["last_response"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            failed(
                config,
                format!(
                    "last_response `{}`; expected accumulated spoken text",
                    reply["state"]["last_response"]
                ),
            )
        })
}

fn require_delivery_acknowledgement(
    config: &LlmProviderConfig,
    reply: &Value,
    final_response: bool,
) -> Result<()> {
    if reply["decision"] == "assistant_responded"
        && reply["state"]["app_response_finalized"] == final_response
    {
        return Ok(());
    }
    Err(failed(
        config,
        format!(
            "speech decision `{}` with finalized={}; expected assistant_responded with final={final_response}",
            reply["decision"], reply["state"]["app_response_finalized"]
        ),
    ))
}

fn failed(config: &LlmProviderConfig, message: String) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: config.provider_id.clone(),
        message,
    }
}

// Wait for the character after punctuation: a token ending in `3.` may continue
// as `14`. Long prose can flush at a word boundary without splitting a word.
fn speech_boundary(text: &str) -> Option<usize> {
    let mut previous = None;
    for (count, (index, ch)) in text.char_indices().enumerate() {
        if ch.is_whitespace()
            && (matches!(previous, Some('.' | '!' | '?' | '。' | '！' | '？'))
                || count >= SPEECH_CHUNK_MAX_CHARS)
        {
            return Some(index + ch.len_utf8());
        }
        previous = Some(ch);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::speech_boundary;

    #[test]
    fn sentence_boundaries_preserve_partial_words_decimals_and_unicode() {
        for text in ["Hello.", "Value 3.", "Value 3.14", "https://a.b/test"] {
            assert_eq!(speech_boundary(text), None, "{text}");
        }
        for (text, first) in [
            ("Hello. Next", "Hello. "),
            ("Hi 🐼! Next", "Hi 🐼! "),
            ("Value 3.14. Next", "Value 3.14. "),
        ] {
            assert_eq!(speech_boundary(text), Some(first.len()));
        }
        let long = format!("{} next", "word ".repeat(60));
        assert!(speech_boundary(&long).unwrap() < long.len());
    }
}
