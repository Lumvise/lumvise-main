use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmStreamEvent;
use crate::llm_providers::command_runner::{response, selected_model};
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmStreamEventSink};
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::native_tool_call_recovery;
use crate::llm_providers::tool_invocation::{
    McpToolCatalog, schema_adapters::to_openai_function_tool, tool_outcome_value,
};
use crate::llm_providers::{LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmResponse};
use crate::process::StreamControl;
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct OpenAiDirectApiTransport {
    config: LlmProviderConfig,
    http_client: Arc<dyn LlmHttpClient>,
}

impl OpenAiDirectApiTransport {
    pub(crate) fn new(config: LlmProviderConfig, http_client: Arc<dyn LlmHttpClient>) -> Self {
        Self {
            config,
            http_client,
        }
    }

    pub(crate) fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        validate_llm_request(request)?;
        if !request.mcp_servers.is_empty() {
            return self.complete_with_mcp(request);
        }
        let value = self
            .http_client
            .post_json(&self.http_request(request, false)?)?;
        let content = openai_response_text(&self.config, &value)?;
        Ok(response(
            &self.config,
            request,
            content,
            response_metadata(&value),
        ))
    }

    pub(crate) fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        validate_llm_request(request)?;
        if !request.mcp_servers.is_empty() {
            let response = self.complete_with_mcp(request)?;
            if !response.content.trim().is_empty() {
                on_event(LlmStreamEvent::ContentDelta {
                    text: response.content,
                })?;
            }
            return on_event(LlmStreamEvent::Complete);
        }
        let mut state = OpenAiApiStreamState::new(control);
        self.http_client
            .stream_text_with_chunks(&self.http_request(request, true)?, &mut |chunk| {
                state.push_chunk(&chunk, on_event)
            })?;
        state.finish(on_event)
    }

    fn http_request(&self, request: &LlmRequest, stream: bool) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: None,
            endpoint: openai_responses_endpoint(&self.config),
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: BTreeMap::new(),
            payload: openai_payload(&self.config, request, stream),
        })
    }

    fn complete_with_mcp(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let tools = McpToolCatalog::discover(&self.config.provider_id, &request.mcp_servers)?;
        let mut input = openai_input(request);
        for _ in 0..4 {
            let payload = openai_payload_with_input(
                &self.config,
                request,
                false,
                input.clone(),
                Some(&tools),
            );
            let value = self
                .http_client
                .post_json(&self.http_request_with_payload(payload)?)?;
            if !append_openai_tool_output(&tools, &mut input, &value)? {
                let content = openai_response_text(&self.config, &value)?;
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
                            response_metadata(&value),
                        ));
                    }
                    native_tool_call_recovery::TextRecovery::Recovered(call) => {
                        append_recovered_openai_tool_call(&mut input, &call);
                    }
                }
            }
        }
        Err(NeuralError::ToolRoundsExhausted {
            provider_id: self.config.provider_id.clone(),
        })
    }

    fn http_request_with_payload(&self, payload: Value) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: None,
            endpoint: openai_responses_endpoint(&self.config),
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: BTreeMap::new(),
            payload,
        })
    }
}

fn openai_responses_endpoint(config: &LlmProviderConfig) -> String {
    let endpoint = config
        .endpoint
        .as_deref()
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/');
    if endpoint.ends_with("/responses") {
        return endpoint.to_string();
    }
    if endpoint.ends_with("/v1") {
        return format!("{endpoint}/responses");
    }
    format!("{endpoint}/v1/responses")
}

fn openai_payload(config: &LlmProviderConfig, request: &LlmRequest, stream: bool) -> Value {
    openai_payload_with_input(config, request, stream, openai_input(request), None)
}

fn openai_payload_with_input(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    stream: bool,
    input: Vec<Value>,
    tools: Option<&McpToolCatalog>,
) -> Value {
    let mut payload = json!({
        "model": selected_model(config, request),
        "stream": stream,
        "input": input,
    });
    if let Some(previous_response_id) = request.provider_session_id.as_deref() {
        payload["previous_response_id"] = json!(previous_response_id);
    }
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        payload["tools"] = json!(
            tools
                .tools()
                .iter()
                .map(to_openai_function_tool)
                .collect::<Vec<_>>()
        );
    }
    payload
}

fn append_openai_tool_output(
    tools: &McpToolCatalog,
    input: &mut Vec<Value>,
    value: &Value,
) -> Result<bool> {
    let Some(call) = openai_function_call(value) else {
        return Ok(false);
    };
    let name = call["name"].as_str().unwrap_or_default();
    let arguments = openai_call_arguments(call)?;
    let result = tool_outcome_value(tools.invoker().invoke(name, arguments))?;
    input.push(json!({
        "type": "function_call_output",
        "call_id": call["call_id"].as_str().unwrap_or_default(),
        "output": serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
    }));
    Ok(true)
}

/// Appends the same `function_call`/`function_call_output` item pair the
/// Responses API tool-call protocol would produce, for a call recovered
/// from Gemma's native serialization instead of a structured
/// `function_call` output item. The synthetic `function_call` item is
/// required alongside the output: unlike the chat-message providers, the
/// Responses API tracks pending calls server-side by `call_id`, so an
/// output with no matching call the server issued would be rejected.
fn append_recovered_openai_tool_call(
    input: &mut Vec<Value>,
    call: &native_tool_call_recovery::RecoveredCall,
) {
    const RECOVERED_CALL_ID: &str = "native-recovered-1";
    input.push(json!({
        "type": "function_call",
        "call_id": RECOVERED_CALL_ID,
        "name": call.name,
        "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string()),
    }));
    input.push(json!({
        "type": "function_call_output",
        "call_id": RECOVERED_CALL_ID,
        "output": serde_json::to_string(&call.result).unwrap_or_else(|_| "{}".to_string()),
    }));
}

fn openai_function_call(value: &Value) -> Option<&Value> {
    value["output"]
        .as_array()?
        .iter()
        .find(|item| item["type"] == "function_call")
}

fn openai_call_arguments(call: &Value) -> Result<Value> {
    let raw = call["arguments"].as_str().unwrap_or("{}");
    serde_json::from_str(raw).map_err(|source| NeuralError::Json {
        value: raw.to_string(),
        expected: "OpenAI direct API function call JSON arguments".to_string(),
        source,
    })
}

fn openai_input(request: &LlmRequest) -> Vec<Value> {
    request
        .messages
        .iter()
        .map(|message| {
            json!({
                "role": message.role,
                "content": openai_content(request, &message.content),
            })
        })
        .collect()
}

fn openai_content(request: &LlmRequest, text: &str) -> Vec<Value> {
    let mut content = vec![json!({ "type": "input_text", "text": text })];
    content.extend(request.modality_inputs.iter().filter_map(openai_image_part));
    content
}

fn openai_image_part(input: &LlmModalityInput) -> Option<Value> {
    if input.kind != LlmModalityInputKind::ImageSnapshot {
        return None;
    }
    Some(json!({
        "type": "input_image",
        "image_url": data_url(&input.media_type, &input.bytes),
    }))
}

fn data_url(media_type: &str, bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{media_type};base64,{encoded}")
}

fn response_metadata(value: &Value) -> Value {
    let mut metadata = json!({ "transport": "direct_api" });
    if let Some(id) = value["id"].as_str() {
        metadata["provider_session_id"] = json!(id);
    }
    metadata
}

fn openai_response_text(config: &LlmProviderConfig, value: &Value) -> Result<String> {
    if let Some(text) = value["output_text"]
        .as_str()
        .filter(|text| !text.is_empty())
    {
        return Ok(text.to_string());
    }
    let text = value["output"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.trim().is_empty() {
        return Ok(text);
    }
    Err(NeuralError::MalformedPayload {
        value: value.to_string(),
        expected: format!("OpenAI response text for {}", config.provider_id),
    })
}

struct OpenAiApiStreamState {
    buffer: String,
    control: StreamControl,
    event_count: usize,
    completed: bool,
}

impl OpenAiApiStreamState {
    fn new(control: StreamControl) -> Self {
        Self {
            buffer: String::new(),
            control,
            event_count: 0,
            completed: false,
        }
    }

    fn push_chunk(&mut self, chunk: &str, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        self.buffer.push_str(chunk);
        self.drain_events(on_event)
    }

    fn finish(&mut self, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        if !self.buffer.trim().is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.drain_event(&event, on_event)?;
        }
        if !self.completed {
            self.emit(LlmStreamEvent::Complete, on_event)?;
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
        if data == "[DONE]" {
            return self.emit(LlmStreamEvent::Complete, on_event);
        }
        let value: Value = serde_json::from_str(data).map_err(|source| NeuralError::Json {
            value: data.to_string(),
            expected: "OpenAI Responses SSE JSON data".to_string(),
            source,
        })?;
        if value["type"] == "response.completed" {
            self.emit_session(&value, on_event)?;
            return self.emit(LlmStreamEvent::Complete, on_event);
        }
        if let Some(text) = value["delta"].as_str().filter(|text| !text.is_empty()) {
            return self.emit(LlmStreamEvent::ContentDelta { text: text.into() }, on_event);
        }
        Ok(())
    }

    fn emit_session(&self, value: &Value, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        if let Some(id) = value["response"]["id"].as_str() {
            return on_event(LlmStreamEvent::Session {
                provider_session_id: id.to_string(),
            });
        }
        Ok(())
    }

    fn emit(&mut self, event: LlmStreamEvent, on_event: &mut LlmStreamEventSink<'_>) -> Result<()> {
        self.completed |= matches!(event, LlmStreamEvent::Complete);
        on_event(event)?;
        self.event_count += 1;
        if self.control.should_cancel_after(self.event_count) {
            self.completed = true;
            return on_event(LlmStreamEvent::Cancelled);
        }
        Ok(())
    }
}
