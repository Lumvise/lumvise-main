use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmStreamEvent;
use crate::llm_providers::command_runner::{response, selected_model};
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmStreamEventSink};
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::native_tool_call_recovery;
use crate::llm_providers::tool_invocation::{
    McpToolCatalog, ToolOutcome, schema_adapters::to_anthropic_tool,
};
use crate::llm_providers::{
    LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmResponse, LlmResponseFormat,
};
use crate::process::StreamControl;
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Anthropic requires `max_tokens`. A generous ceiling that every current
/// Claude model accepts: the request deadline, not this cap, bounds runaway
/// output, and only generated tokens are billed.
const DEFAULT_MAX_TOKENS: u32 = 16_384;

pub(crate) struct ClaudeDirectApiTransport {
    config: LlmProviderConfig,
    http_client: Arc<dyn LlmHttpClient>,
}

impl ClaudeDirectApiTransport {
    pub(crate) fn new(config: LlmProviderConfig, http_client: Arc<dyn LlmHttpClient>) -> Self {
        Self {
            config,
            http_client,
        }
    }

    pub(crate) fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        self.complete_with_control(request, StreamControl::unbounded())
    }

    pub(crate) fn complete_with_control(
        &self,
        request: &LlmRequest,
        control: StreamControl,
    ) -> Result<LlmResponse> {
        validate_llm_request(request)?;
        if !request.mcp_servers.is_empty() {
            return self.complete_with_mcp(request, &control);
        }
        let value = self
            .http_client
            .post_json(&self.http_request(request, false, &control)?)?;
        let content = claude_response_text(&self.config, &value)?;
        Ok(response(&self.config, request, content, json!({})))
    }

    pub(crate) fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        validate_llm_request(request)?;
        if !request.mcp_servers.is_empty() {
            let response = self.complete_with_mcp(request, &control)?;
            if !response.content.trim().is_empty() {
                on_event(LlmStreamEvent::ContentDelta {
                    text: response.content,
                })?;
            }
            return on_event(LlmStreamEvent::Complete);
        }
        let mut state = ClaudeApiStreamState::new(control.clone());
        self.http_client.stream_text_with_chunks(
            &self.http_request(request, true, &control)?,
            &mut |chunk| state.push_chunk(&chunk, on_event),
        )?;
        state.finish(on_event)
    }

    fn http_request(
        &self,
        request: &LlmRequest,
        stream: bool,
        control: &StreamControl,
    ) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: control.remaining(),
            endpoint: claude_messages_endpoint(&self.config),
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: claude_headers(&self.config),
            payload: claude_payload(&self.config, request, stream),
        })
    }

    fn complete_with_mcp(
        &self,
        request: &LlmRequest,
        control: &StreamControl,
    ) -> Result<LlmResponse> {
        let tools = McpToolCatalog::discover(&self.config.provider_id, &request.mcp_servers)?;
        let mut messages = claude_messages(request);
        for _ in 0..4 {
            let payload = claude_payload_with_messages(
                &self.config,
                request,
                false,
                messages.clone(),
                Some(&tools),
            );
            let value = self
                .http_client
                .post_json(&self.http_request_with_payload(payload, control)?)?;
            if !append_claude_tool_result(&tools, &mut messages, &value)? {
                let content = claude_response_text(&self.config, &value)?;
                match native_tool_call_recovery::recover_or_confirm_final_text(
                    self.config.kind,
                    &selected_model(&self.config, request),
                    content,
                    &tools,
                )? {
                    native_tool_call_recovery::TextRecovery::FinalText(content) => {
                        return Ok(response(
                            &self.config,
                            request,
                            content,
                            json!({ "tool_bridge": "mcp", "transport": "direct_api" }),
                        ));
                    }
                    native_tool_call_recovery::TextRecovery::Recovered(call) => {
                        append_recovered_claude_tool_call(&mut messages, &call);
                    }
                }
            }
        }
        Err(NeuralError::ToolRoundsExhausted {
            provider_id: self.config.provider_id.clone(),
        })
    }

    fn http_request_with_payload(
        &self,
        payload: Value,
        control: &StreamControl,
    ) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: control.remaining(),
            endpoint: claude_messages_endpoint(&self.config),
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: claude_headers(&self.config),
            payload,
        })
    }
}

fn claude_headers(config: &LlmProviderConfig) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
    headers.insert(
        "x-api-key".to_string(),
        config.credential.clone().unwrap_or_default(),
    );
    headers
}

fn claude_messages_endpoint(config: &LlmProviderConfig) -> String {
    let endpoint = config
        .endpoint
        .as_deref()
        .unwrap_or("https://api.anthropic.com")
        .trim_end_matches('/');
    if endpoint.ends_with("/messages") {
        return endpoint.to_string();
    }
    if endpoint.ends_with("/v1") {
        return format!("{endpoint}/messages");
    }
    format!("{endpoint}/v1/messages")
}

fn claude_payload(config: &LlmProviderConfig, request: &LlmRequest, stream: bool) -> Value {
    claude_payload_with_messages(config, request, stream, claude_messages(request), None)
}

fn claude_payload_with_messages(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    stream: bool,
    messages: Vec<Value>,
    tools: Option<&McpToolCatalog>,
) -> Value {
    let mut payload = json!({
        "model": selected_model(config, request),
        "max_tokens": request.options.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "stream": stream,
        "messages": messages,
    });
    if let Some(temperature) = request.options.temperature {
        payload["temperature"] = json!(temperature);
    }
    if let Some(format) = &request.options.response_format
        && let Some(system) = claude_json_system_prompt(format)
    {
        payload["system"] = json!(system);
    }
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        payload["tools"] = json!(
            tools
                .tools()
                .iter()
                .map(to_anthropic_tool)
                .collect::<Vec<_>>()
        );
    }
    payload
}

/// Anthropic Messages has no JSON mode on this transport; a JSON reply is
/// requested through the system prompt instead.
fn claude_json_system_prompt(format: &LlmResponseFormat) -> Option<String> {
    match format {
        LlmResponseFormat::JsonObject => Some("Reply with only one JSON object.".to_string()),
        LlmResponseFormat::JsonSchema { schema, .. } => Some(format!(
            "Reply with only one JSON object matching this JSON Schema: {schema}."
        )),
    }
}

fn append_claude_tool_result(
    tools: &McpToolCatalog,
    messages: &mut Vec<Value>,
    value: &Value,
) -> Result<bool> {
    let Some(tool_use) = claude_tool_use(value) else {
        return Ok(false);
    };
    messages.push(json!({ "role": "assistant", "content": value["content"].clone() }));
    let (result, is_error) = match tools.invoker().invoke(
        tool_use["name"].as_str().unwrap_or_default(),
        tool_use["input"].clone(),
    ) {
        ToolOutcome::Success(result) => (result, false),
        ToolOutcome::ToolError(result) => (result, true),
        ToolOutcome::Validation(message) => (json!({ "error": { "message": message } }), true),
        ToolOutcome::Transport(error) => return Err(error),
    };
    let mut tool_result = json!({
        "type": "tool_result",
        "tool_use_id": tool_use["id"].as_str().unwrap_or_default(),
        "content": serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
    });
    if is_error {
        tool_result["is_error"] = json!(true);
    }
    messages.push(json!({ "role": "user", "content": [tool_result] }));
    Ok(true)
}

/// Appends the same `tool_use`/`tool_result` content-block pair the Claude
/// tool-call protocol would produce, for a call recovered from Gemma's
/// native serialization instead of a structured `tool_use` block.
fn append_recovered_claude_tool_call(
    messages: &mut Vec<Value>,
    call: &native_tool_call_recovery::RecoveredCall,
) {
    const RECOVERED_CALL_ID: &str = "native-recovered-1";
    messages.push(json!({ "role": "assistant", "content": [{
        "type": "tool_use", "id": RECOVERED_CALL_ID,
        "name": call.name, "input": call.arguments,
    }] }));
    messages.push(json!({ "role": "user", "content": [{
        "type": "tool_result", "tool_use_id": RECOVERED_CALL_ID,
        "content": serde_json::to_string(&call.result).unwrap_or_else(|_| "{}".to_string()),
    }] }));
}

fn claude_tool_use(value: &Value) -> Option<&Value> {
    value["content"]
        .as_array()?
        .iter()
        .find(|part| part["type"] == "tool_use")
}

fn claude_messages(request: &LlmRequest) -> Vec<Value> {
    request
        .messages
        .iter()
        .map(|message| {
            if message.role == "user" && !request.modality_inputs.is_empty() {
                return json!({ "role": message.role, "content": claude_content(request, &message.content) });
            }
            json!({ "role": message.role, "content": message.content })
        })
        .collect()
}

fn claude_content(request: &LlmRequest, text: &str) -> Vec<Value> {
    let mut content = vec![json!({ "type": "text", "text": text })];
    content.extend(request.modality_inputs.iter().filter_map(claude_image_part));
    content
}

fn claude_image_part(input: &LlmModalityInput) -> Option<Value> {
    if input.kind != LlmModalityInputKind::ImageSnapshot {
        return None;
    }
    let data = base64::engine::general_purpose::STANDARD.encode(&input.bytes);
    Some(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": input.media_type,
            "data": data,
        }
    }))
}

fn claude_response_text(config: &LlmProviderConfig, value: &Value) -> Result<String> {
    let text = value["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.trim().is_empty() {
        return Ok(text);
    }
    Err(NeuralError::MalformedPayload {
        value: value.to_string(),
        expected: format!("Claude content text for {}", config.provider_id),
    })
}

struct ClaudeApiStreamState {
    buffer: String,
    control: StreamControl,
    event_count: usize,
    stopped: bool,
}

impl ClaudeApiStreamState {
    fn new(control: StreamControl) -> Self {
        Self {
            buffer: String::new(),
            control,
            event_count: 0,
            stopped: false,
        }
    }

    fn push_chunk(&mut self, chunk: &str, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        self.buffer.push_str(chunk);
        self.drain_events(on_event)
    }

    fn finish(&mut self, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        if !self.stopped && !self.buffer.trim().is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.drain_event(&event, on_event)?;
        }
        Ok(())
    }

    fn drain_events(&mut self, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        while let Some(index) = self.buffer.find("\n\n") {
            let event = self.buffer[..index].to_string();
            self.buffer.drain(..index + 2);
            self.drain_event(&event, on_event)?;
        }
        Ok(())
    }

    fn drain_event(&mut self, event: &str, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        for line in event.lines().filter_map(|line| line.strip_prefix("data:")) {
            self.drain_data(line.trim(), on_event)?;
        }
        Ok(())
    }

    fn drain_data(&mut self, data: &str, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        let value: Value = serde_json::from_str(data).map_err(|source| NeuralError::Json {
            value: data.to_string(),
            expected: "Claude SSE JSON data".to_string(),
            source,
        })?;
        if value["type"] == "message_stop" {
            return self.emit(LlmStreamEvent::Complete, on_event);
        }
        if let Some(text) = value["delta"]["text"].as_str() {
            return self.emit(LlmStreamEvent::ContentDelta { text: text.into() }, on_event);
        }
        Ok(())
    }

    fn emit(&mut self, event: LlmStreamEvent, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        on_event(event)?;
        self.event_count += 1;
        if self.control.should_cancel_after(self.event_count) {
            self.stopped = true;
            return on_event(LlmStreamEvent::Cancelled);
        }
        Ok(())
    }
}
