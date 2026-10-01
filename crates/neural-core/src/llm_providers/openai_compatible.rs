use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::command_runner::selected_model;
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmStreamEventSink};
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::native_tool_call_recovery;
use crate::llm_providers::openai_compatible_stream::OpenAiCompatibleStreamState;
use crate::llm_providers::tool_invocation::{
    McpToolCatalog, schema_adapters::to_openai_chat_function_tool, tool_outcome_value,
};
use crate::llm_providers::{
    LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmResponse, LlmResponseFormat,
    LlmStreamEvent,
};
use crate::process::StreamControl;
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

mod catalog;
mod tool_stream;
mod turn_completion;

#[derive(Debug, Clone, Copy)]
pub(crate) struct OpenAiCompatibleProviderSpec {
    pub(crate) label: &'static str,
    pub(crate) supports_image_snapshot: bool,
    pub(crate) tool_call_limit: usize,
}

pub(crate) struct OpenAiCompatibleChatProvider {
    config: LlmProviderConfig,
    http_client: Arc<dyn LlmHttpClient>,
    spec: OpenAiCompatibleProviderSpec,
    tool_catalog: catalog::ConversationToolCatalog,
}

impl OpenAiCompatibleChatProvider {
    pub(crate) fn new(
        config: LlmProviderConfig,
        http_client: Arc<dyn LlmHttpClient>,
        spec: OpenAiCompatibleProviderSpec,
    ) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            http_client,
            spec,
            tool_catalog: catalog::ConversationToolCatalog::default(),
        })
    }

    pub(crate) fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    pub(crate) fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        self.complete_with_control(request, StreamControl::unbounded())
    }

    /// Runs one completion bounded by `control`: every HTTP round carries
    /// the control's remaining time as its request timeout, so a caller
    /// deadline expires in the transport instead of occupying a provider
    /// worker lane for the full client timeout.
    pub(crate) fn complete_with_control(
        &self,
        request: &LlmRequest,
        control: StreamControl,
    ) -> Result<LlmResponse> {
        validate_llm_request(request)?;
        self.validate_modality_inputs(request)?;
        if !request.mcp_servers.is_empty() {
            return self.complete_with_mcp_tools(request, &control);
        }
        let response = self.tool_round(request, self.messages(request), None, &control)?;
        self.response_from_value(request, response)
    }

    pub(crate) fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        validate_llm_request(request)?;
        self.validate_modality_inputs(request)?;
        if !request.mcp_servers.is_empty() {
            return self.stream_with_mcp_tools(request, control, on_event);
        }
        let mut state = OpenAiCompatibleStreamState::new(control.clone(), self.spec.label);
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
        self.http_request_with_payload(
            self.payload(request, stream, self.messages(request), None),
            control,
        )
    }

    fn http_request_with_payload(
        &self,
        payload: Value,
        control: &StreamControl,
    ) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: control.remaining(),
            endpoint: chat_completions_endpoint(&self.config)?,
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: BTreeMap::new(),
            payload,
        })
    }

    fn complete_with_mcp_tools(
        &self,
        request: &LlmRequest,
        control: &StreamControl,
    ) -> Result<LlmResponse> {
        check_tool_cancellation(&self.config, control)?;
        let tools = self
            .tool_catalog
            .resolve(&self.config.provider_id, request)?;
        let result = self.tool_loop(request, &tools, control);
        if result.is_err() {
            self.tool_catalog.invalidate(&tools);
        }
        let content = result?;
        Ok(LlmResponse {
            provider_id: self.config.provider_id.clone(),
            model: selected_model(&self.config, request),
            content,
            metadata: json!({ "tool_bridge": "mcp", "transport": "direct_api" }),
        })
    }

    fn stream_with_mcp_tools(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        let response = match self.complete_with_mcp_tools(request, &control) {
            Err(NeuralError::ProcessCancelled { .. }) => {
                return on_event(LlmStreamEvent::Cancelled);
            }
            result => result?,
        };
        emit_complete_response(response.content, control, on_event)
    }

    fn tool_loop(
        &self,
        request: &LlmRequest,
        tools: &McpToolCatalog,
        control: &StreamControl,
    ) -> Result<String> {
        let mut messages = self.messages(request);
        for _ in 0..self.spec.tool_call_limit {
            check_tool_cancellation(&self.config, control)?;
            let response = self.tool_round(
                request,
                messages.clone(),
                Some(openai_tool_descriptors(tools)),
                control,
            )?;
            check_tool_cancellation(&self.config, control)?;
            if response["choices"][0]["finish_reason"] == "length" {
                return Err(NeuralError::ProviderFailed {
                    provider_id: self.config.provider_id.clone(),
                    message: "Response reached the output token limit before completing the turn. Try a smaller request.".into(),
                });
            }
            if append_openai_tool_calls(&self.config, tools, &mut messages, &response, control)? {
                if let Some(content) = turn_completion::finalized_response(&messages) {
                    return Ok(content);
                }
                continue;
            } else {
                let content = required_choice_content(&response, "message", self.spec.label)?;
                match native_tool_call_recovery::recover_or_confirm_final_text(
                    self.config.kind,
                    &selected_model(&self.config, request),
                    content,
                    tools,
                )? {
                    native_tool_call_recovery::TextRecovery::FinalText(content) => {
                        return Ok(content);
                    }
                    native_tool_call_recovery::TextRecovery::Recovered(call) => {
                        append_recovered_tool_call(&mut messages, &call);
                        if let Some(content) = turn_completion::finalized_response(&messages) {
                            return Ok(content);
                        }
                    }
                }
            }
        }
        // Removing tools for one final request contradicts the Assistant's
        // respond/draw contract and can turn a valid canvas call into raw XML.
        Err(NeuralError::ToolRoundsExhausted {
            provider_id: self.config.provider_id.clone(),
        })
    }

    fn tool_round(
        &self,
        request: &LlmRequest,
        messages: Vec<Value>,
        tools: Option<Value>,
        control: &StreamControl,
    ) -> Result<Value> {
        let started = std::time::Instant::now();
        let streaming = self.config.kind == crate::config::LlmProviderKind::OpenRouter;
        let http_request = self.http_request_with_payload(
            self.payload(request, streaming, messages, tools),
            control,
        )?;
        if !streaming {
            return self.http_client.post_json(&http_request);
        }
        let mut completion = tool_stream::ToolCompletionStream::default();
        let mut received_bytes = 0_usize;
        let mut chunks = 0_usize;
        let mut first_chunk_ms = None;
        let mut last_chunk_ms = None;
        let mut last_progress_ms = 0;
        tracing::debug!(provider_id = %self.config.provider_id,
            conversation_id = ?request.conversation_id,
            reasoning_effort = ?request.options.reasoning_effort,
            request_bytes = http_request.payload.to_string().len(),
            messages = http_request.payload["messages"].as_array().map_or(0, Vec::len),
            "streamed model request started");
        let streamed = self
            .http_client
            .stream_text_until(&http_request, &mut |chunk| {
                let elapsed_ms = started.elapsed().as_millis();
                first_chunk_ms.get_or_insert(elapsed_ms);
                last_chunk_ms = Some(elapsed_ms);
                received_bytes += chunk.len();
                chunks += 1;
                check_tool_cancellation(&self.config, control)?;
                let complete = completion.push(&chunk)?;
                if chunks == 1 || elapsed_ms - last_progress_ms >= 10_000 {
                    tracing::debug!(provider_id = %self.config.provider_id,
                        conversation_id = ?request.conversation_id, elapsed_ms,
                        received_bytes, chunks, stream = %completion.diagnostic_snapshot(),
                        "model stream progress");
                    last_progress_ms = elapsed_ms;
                }
                Ok(if complete {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                })
            });
        if streamed.is_err() {
            tracing::warn!(provider_id = %self.config.provider_id,
                conversation_id = ?request.conversation_id,
                elapsed_ms = started.elapsed().as_millis(),
                received_bytes, chunks, ?first_chunk_ms, ?last_chunk_ms,
                stream = %completion.diagnostic_snapshot(),
                "streamed model request failed before turn completion");
        }
        streamed?;
        let response = completion.finish()?;
        tracing::debug!(provider_id = %self.config.provider_id,
            conversation_id = ?request.conversation_id,
            elapsed_ms = started.elapsed().as_millis(),
            received_bytes, chunks, ?first_chunk_ms, ?last_chunk_ms,
            finish_reason = %response["choices"][0]["finish_reason"],
            "streamed tool round completed");
        Ok(response)
    }

    fn payload(
        &self,
        request: &LlmRequest,
        stream: bool,
        messages: Vec<Value>,
        tools: Option<Value>,
    ) -> Value {
        let mut payload = json!({
            "model": selected_model(&self.config, request),
            "stream": stream,
            "messages": messages,
        });
        if self.config.kind == crate::config::LlmProviderKind::OpenRouter
            && let Some(session_id) = request.conversation_id.as_deref()
        {
            payload["session_id"] = json!(session_id);
        }
        if let Some(tools) = tools
            .as_ref()
            .filter(|tools| !tools.as_array().is_some_and(Vec::is_empty))
        {
            payload["tools"] = tools.clone();
            payload["tool_choice"] = json!("auto");
        }
        let options = &request.options;
        if self.config.kind == crate::config::LlmProviderKind::OpenRouter
            && let Some(effort) = options.reasoning_effort
        {
            payload["reasoning"] = json!({"effort": effort});
        }
        if let Some(max_output_tokens) = options.max_output_tokens {
            payload["max_tokens"] = json!(max_output_tokens);
        }
        if let Some(temperature) = options.temperature {
            payload["temperature"] = json!(temperature);
        }
        // Tool rounds negotiate function calls; a JSON response format would
        // make the model emit JSON instead of structured tool_calls.
        if tools.is_none()
            && let Some(response_format) = openai_response_format(&options.response_format)
        {
            payload["response_format"] = response_format;
        }
        payload
    }

    fn messages(&self, request: &LlmRequest) -> Vec<Value> {
        if request.modality_inputs.is_empty() {
            return request
                .messages
                .iter()
                .map(|message| json!(message))
                .collect();
        }
        request
            .messages
            .iter()
            .map(|message| self.multimodal_message(request, message))
            .collect()
    }

    fn multimodal_message(
        &self,
        request: &LlmRequest,
        message: &crate::llm_providers::LlmMessage,
    ) -> Value {
        if message.role != "user" {
            return json!(message);
        }
        let mut content = vec![json!({ "type": "text", "text": message.content })];
        content.extend(request.modality_inputs.iter().filter_map(modality_part));
        json!({ "role": message.role, "content": content })
    }

    fn validate_modality_inputs(&self, request: &LlmRequest) -> Result<()> {
        for input in &request.modality_inputs {
            self.validate_modality_input(input)?;
        }
        Ok(())
    }

    fn validate_modality_input(&self, input: &LlmModalityInput) -> Result<()> {
        match input.kind {
            LlmModalityInputKind::ImageSnapshot if self.spec.supports_image_snapshot => Ok(()),
            LlmModalityInputKind::ImageSnapshot => {
                Err(self
                    .unsupported_modality(&input.input_id, "image_snapshot_input is unsupported"))
            }
            LlmModalityInputKind::LiveAudioChunk => {
                Err(self.unsupported_modality(&input.input_id, "live_audio_input is unsupported"))
            }
            LlmModalityInputKind::ScreenFrame => Err(self.unsupported_modality(
                &input.input_id,
                "screen_frame_broadcast_input is unsupported",
            )),
        }
    }

    fn unsupported_modality(&self, input_id: &str, expected: &str) -> NeuralError {
        let image_hint = if self.spec.supports_image_snapshot {
            "image_snapshot_input or text-only request"
        } else {
            "text-only request"
        };
        NeuralError::InvalidValue {
            value: format!("{}:{input_id}", self.config.provider_id),
            expected: format!("{} {image_hint}; {expected}", self.spec.label),
        }
    }

    fn response_from_value(&self, request: &LlmRequest, value: Value) -> Result<LlmResponse> {
        let content = required_choice_content(&value, "message", self.spec.label)?;
        if content.trim().is_empty() {
            return Err(malformed_payload(
                value,
                &format!("non-empty {} content", self.spec.label),
            ));
        }
        let content = native_tool_call_recovery::reject_undispatchable_call_attempt(
            self.config.kind,
            &selected_model(&self.config, request),
            content,
        )?;
        Ok(LlmResponse {
            provider_id: self.config.provider_id.clone(),
            model: selected_model(&self.config, request),
            content,
            metadata: value,
        })
    }
}

fn emit_complete_response(
    content: String,
    control: StreamControl,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    if content.trim().is_empty() {
        return on_event(LlmStreamEvent::Complete);
    }
    on_event(LlmStreamEvent::ContentDelta { text: content })?;
    if control.should_cancel_after(1) {
        return on_event(LlmStreamEvent::Cancelled);
    }
    on_event(LlmStreamEvent::Complete)
}

fn openai_response_format(response_format: &Option<LlmResponseFormat>) -> Option<Value> {
    match response_format {
        Some(LlmResponseFormat::JsonObject) => Some(json!({ "type": "json_object" })),
        Some(LlmResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        }) => Some(json!({
            "type": "json_schema",
            "json_schema": { "name": name, "schema": schema, "strict": strict },
        })),
        None => None,
    }
}

fn chat_completions_endpoint(config: &LlmProviderConfig) -> Result<String> {
    let endpoint = config.endpoint.as_deref().unwrap_or_default().trim();
    if endpoint.ends_with("/chat/completions") {
        return Ok(endpoint.to_string());
    }
    Ok(format!(
        "{}/chat/completions",
        endpoint.trim_end_matches('/')
    ))
}

fn append_openai_tool_calls(
    config: &LlmProviderConfig,
    tools: &McpToolCatalog,
    messages: &mut Vec<Value>,
    response: &Value,
    control: &StreamControl,
) -> Result<bool> {
    let Some(message) = response["choices"][0]["message"].as_object() else {
        return Ok(false);
    };
    let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return Ok(false);
    };
    if tool_calls.is_empty() {
        return Ok(false);
    }
    messages.push(Value::Object(message.clone()));
    append_tool_results(config, tools, messages, tool_calls, control)?;
    Ok(true)
}

/// Appends the same `assistant` + `tool` message pair the OpenAI tool-call
/// protocol would produce, for a call recovered from Gemma's native
/// serialization instead of a structured `tool_calls` response - so the
/// next round trip sees an identical conversation shape either way.
fn append_recovered_tool_call(
    messages: &mut Vec<Value>,
    call: &native_tool_call_recovery::RecoveredCall,
) {
    const RECOVERED_CALL_ID: &str = "native-recovered-1";
    messages.push(json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{
            "id": RECOVERED_CALL_ID,
            "type": "function",
            "function": { "name": call.name, "arguments": call.arguments.to_string() },
        }],
    }));
    messages.push(json!({
        "role": "tool",
        "tool_call_id": RECOVERED_CALL_ID,
        "content": serde_json::to_string(&call.result).unwrap_or_else(|_| "{}".to_string()),
    }));
}

fn append_tool_results(
    config: &LlmProviderConfig,
    tools: &McpToolCatalog,
    messages: &mut Vec<Value>,
    tool_calls: &[Value],
    control: &StreamControl,
) -> Result<()> {
    for call in tool_calls {
        check_tool_cancellation(config, control)?;
        messages.push(execute_tool_call(config, tools, call)?);
    }
    Ok(())
}

fn execute_tool_call(
    config: &LlmProviderConfig,
    tools: &McpToolCatalog,
    call: &Value,
) -> Result<Value> {
    let name = tool_call_name(config, call)?;
    let id = call["id"].as_str().unwrap_or(&name);
    let result = tool_outcome_value(
        tools
            .invoker()
            .invoke(&name, tool_call_arguments(config, call)?),
    )?;
    Ok(json!({
        "role": "tool",
        "tool_call_id": id,
        "content": serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
    }))
}

fn tool_call_name(config: &LlmProviderConfig, call: &Value) -> Result<String> {
    call["function"]["name"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            malformed_payload(
                call.clone(),
                &format!("{} tool call name", config.provider_id),
            )
        })
}

fn tool_call_arguments(config: &LlmProviderConfig, call: &Value) -> Result<Value> {
    let raw = call["function"]["arguments"].as_str().unwrap_or("{}");
    serde_json::from_str(raw).map_err(|source| NeuralError::Json {
        value: raw.to_string(),
        expected: format!("{} tool call JSON arguments", config.provider_id),
        source,
    })
}

fn openai_tool_descriptors(tools: &McpToolCatalog) -> Value {
    Value::Array(
        tools
            .tools()
            .iter()
            .map(to_openai_chat_function_tool)
            .collect(),
    )
}

fn modality_part(input: &LlmModalityInput) -> Option<Value> {
    match input.kind {
        LlmModalityInputKind::ImageSnapshot => Some(json!({
            "type": "image_url",
            "image_url": { "url": data_url(&input.media_type, &input.bytes) }
        })),
        LlmModalityInputKind::LiveAudioChunk | LlmModalityInputKind::ScreenFrame => None,
    }
}

fn data_url(media_type: &str, bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{media_type};base64,{encoded}")
}

fn required_choice_content(value: &Value, field: &str, provider_label: &str) -> Result<String> {
    let content = choice_content_value(value, field).ok_or_else(|| {
        malformed_payload(
            value.clone(),
            &format!("{provider_label} choices[0] content"),
        )
    })?;
    content_text(content)
        .ok_or_else(|| malformed_payload(value.clone(), &format!("{provider_label} content text")))
}

fn choice_content_value<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    value
        .get("choices")?
        .as_array()?
        .first()?
        .get(field)?
        .get("content")
}

fn content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let text = content
        .as_array()?
        .iter()
        .filter_map(content_part_text)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn content_part_text(part: &Value) -> Option<String> {
    part.as_str()
        .or_else(|| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn malformed_payload(value: Value, expected: &str) -> NeuralError {
    NeuralError::MalformedPayload {
        value: value.to_string(),
        expected: expected.to_string(),
    }
}

fn check_tool_cancellation(config: &LlmProviderConfig, control: &StreamControl) -> Result<()> {
    if control.is_cancelled() {
        return Err(NeuralError::ProcessCancelled {
            command: config.provider_id.clone(),
        });
    }
    Ok(())
}
