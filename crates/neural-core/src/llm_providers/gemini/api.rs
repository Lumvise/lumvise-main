use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmStreamEvent;
use crate::llm_providers::command_runner::{response, selected_model};
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmStreamEventSink};
use crate::llm_providers::local::validate_llm_request;
use crate::llm_providers::native_tool_call_recovery;
use crate::llm_providers::tool_invocation::{
    McpToolCatalog, schema_adapters::to_gemini_declaration, tool_outcome_value,
};
use crate::llm_providers::{
    LlmModalityInput, LlmModalityInputKind, LlmRequest, LlmResponse, LlmResponseFormat,
};
use crate::process::StreamControl;
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct GeminiDirectApiTransport {
    config: LlmProviderConfig,
    http_client: Arc<dyn LlmHttpClient>,
}

impl GeminiDirectApiTransport {
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
        let value = self.http_client.post_json(&self.http_request(
            request,
            "generateContent",
            &control,
        )?)?;
        let content = gemini_response_text(&self.config, &value)?;
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
        let mut state = GeminiApiStreamState::new(control.clone());
        self.http_client.stream_text_with_chunks(
            &self.http_request(request, "streamGenerateContent", &control)?,
            &mut |chunk| state.push_chunk(&chunk, on_event),
        )?;
        state.finish(on_event)
    }

    fn http_request(
        &self,
        request: &LlmRequest,
        method: &str,
        control: &StreamControl,
    ) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: control.remaining(),
            endpoint: gemini_endpoint(&self.config, request, method),
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: gemini_headers(&self.config),
            payload: gemini_payload(request),
        })
    }

    fn complete_with_mcp(
        &self,
        request: &LlmRequest,
        control: &StreamControl,
    ) -> Result<LlmResponse> {
        let tools = McpToolCatalog::discover(&self.config.provider_id, &request.mcp_servers)?;
        let mut contents = gemini_contents(request);
        for _ in 0..4 {
            let payload = gemini_payload_with_contents(request, contents.clone(), Some(&tools));
            let value = self.http_client.post_json(&self.http_request_with_payload(
                request,
                "generateContent",
                payload,
                control,
            )?)?;
            if !append_gemini_tool_result(&tools, &mut contents, &value)? {
                let content = gemini_response_text(&self.config, &value)?;
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
                        append_recovered_gemini_tool_call(&mut contents, &call);
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
        request: &LlmRequest,
        method: &str,
        payload: Value,
        control: &StreamControl,
    ) -> Result<LlmHttpRequest> {
        Ok(LlmHttpRequest {
            timeout: control.remaining(),
            endpoint: gemini_endpoint(&self.config, request, method),
            credential: self.config.credential.clone().unwrap_or_default(),
            headers: gemini_headers(&self.config),
            payload,
        })
    }
}

fn gemini_headers(config: &LlmProviderConfig) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "x-goog-api-key".to_string(),
        config.credential.clone().unwrap_or_default(),
    );
    headers
}

fn gemini_endpoint(config: &LlmProviderConfig, request: &LlmRequest, method: &str) -> String {
    let endpoint = config
        .endpoint
        .as_deref()
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta")
        .trim_end_matches('/');
    if endpoint.contains(":generateContent") || endpoint.contains(":streamGenerateContent") {
        return endpoint.to_string();
    }
    let model = selected_model(config, request);
    format!("{endpoint}/models/{model}:{method}")
}

fn gemini_payload(request: &LlmRequest) -> Value {
    gemini_payload_with_contents(request, gemini_contents(request), None)
}

fn gemini_payload_with_contents(
    request: &LlmRequest,
    contents: Vec<Value>,
    tools: Option<&McpToolCatalog>,
) -> Value {
    let mut payload = json!({ "contents": contents });
    let options = &request.options;
    let mut generation_config = serde_json::Map::new();
    if let Some(max_output_tokens) = options.max_output_tokens {
        generation_config.insert("maxOutputTokens".into(), json!(max_output_tokens));
    }
    if let Some(temperature) = options.temperature {
        generation_config.insert("temperature".into(), json!(temperature));
    }
    if let Some(format) = &options.response_format {
        generation_config.insert("responseMimeType".into(), json!("application/json"));
        // responseSchema only when the JSON Schema maps to Gemini's subset;
        // otherwise the mime type alone still forces a JSON reply.
        if let Some(schema) = gemini_response_schema(format) {
            generation_config.insert("responseSchema".into(), schema);
        }
    }
    if !generation_config.is_empty() {
        payload["generationConfig"] = Value::Object(generation_config);
    }
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        payload["tools"] = json!([{
            "functionDeclarations": tools.tools().iter().map(to_gemini_declaration).collect::<Vec<_>>()
        }]);
    }
    payload
}

/// Maps a requested format onto Gemini's `responseSchema`. Only the plain
/// `type: object` envelope is trivially compatible; richer schemas would be
/// rejected by the API, so they keep the mime type alone.
fn gemini_response_schema(format: &LlmResponseFormat) -> Option<Value> {
    let LlmResponseFormat::JsonSchema { schema, .. } = format else {
        return None;
    };
    let is_plain_object = schema.as_object().is_some_and(|schema| {
        schema.get("type").and_then(Value::as_str) == Some("object")
            && schema
                .keys()
                .all(|key| matches!(key.as_str(), "type" | "properties" | "required"))
    });
    is_plain_object.then(|| schema.clone())
}

fn gemini_contents(request: &LlmRequest) -> Vec<Value> {
    request
        .messages
        .iter()
        .map(|message| {
            json!({ "role": gemini_role(&message.role), "parts": gemini_parts(request, &message.content) })
        })
        .collect()
}

fn append_gemini_tool_result(
    tools: &McpToolCatalog,
    contents: &mut Vec<Value>,
    value: &Value,
) -> Result<bool> {
    let Some(call) = gemini_function_call(value) else {
        return Ok(false);
    };
    let name = call["name"].as_str().unwrap_or_default();
    contents.push(json!({ "role": "model", "parts": [{ "functionCall": call.clone() }] }));
    let result = tool_outcome_value(tools.invoker().invoke(name, call["args"].clone()))?;
    contents.push(json!({
        "role": "user",
        "parts": [{ "functionResponse": { "name": name, "response": result } }]
    }));
    Ok(true)
}

/// Appends the same `functionCall`/`functionResponse` content pair the
/// Gemini tool-call protocol would produce, for a call recovered from
/// Gemma's native serialization instead of a structured `functionCall`
/// part.
fn append_recovered_gemini_tool_call(
    contents: &mut Vec<Value>,
    call: &native_tool_call_recovery::RecoveredCall,
) {
    contents.push(json!({ "role": "model", "parts": [{
        "functionCall": { "name": call.name, "args": call.arguments },
    }] }));
    contents.push(json!({ "role": "user", "parts": [{
        "functionResponse": { "name": call.name, "response": call.result },
    }] }));
}

fn gemini_function_call(value: &Value) -> Option<&Value> {
    value["candidates"][0]["content"]["parts"]
        .as_array()?
        .iter()
        .find_map(|part| part.get("functionCall"))
}

fn gemini_role(role: &str) -> &str {
    if role == "assistant" {
        return "model";
    }
    "user"
}

fn gemini_parts(request: &LlmRequest, text: &str) -> Vec<Value> {
    let mut parts = vec![json!({ "text": text })];
    parts.extend(
        request
            .modality_inputs
            .iter()
            .filter_map(gemini_inline_part),
    );
    parts
}

fn gemini_inline_part(input: &LlmModalityInput) -> Option<Value> {
    if input.kind != LlmModalityInputKind::ImageSnapshot {
        return None;
    }
    Some(json!({
        "inlineData": {
            "mimeType": input.media_type,
            "data": base64::engine::general_purpose::STANDARD.encode(&input.bytes),
        }
    }))
}

fn gemini_response_text(config: &LlmProviderConfig, value: &Value) -> Result<String> {
    let text = value["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|candidate| {
            candidate["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.trim().is_empty() {
        return Ok(text);
    }
    Err(NeuralError::MalformedPayload {
        value: value.to_string(),
        expected: format!("Gemini candidate text for {}", config.provider_id),
    })
}

struct GeminiApiStreamState {
    buffer: String,
    control: StreamControl,
    event_count: usize,
    completed: bool,
}

impl GeminiApiStreamState {
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
            let value: Value =
                serde_json::from_str(line.trim()).map_err(|source| NeuralError::Json {
                    value: line.trim().to_string(),
                    expected: "Gemini SSE JSON data".to_string(),
                    source,
                })?;
            let text = gemini_response_text_from_stream(&value);
            if !text.is_empty() {
                self.emit(LlmStreamEvent::ContentDelta { text }, on_event)?;
            }
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

fn gemini_response_text_from_stream(value: &Value) -> String {
    value["candidates"][0]["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
