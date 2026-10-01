use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result, require_non_empty};
use crate::llm_providers::capabilities::local_provider_capabilities;
use crate::llm_providers::contract::{LlmProvider, LlmStreamEventSink};
use crate::llm_providers::{
    LlmModalityInputKind, LlmProviderCapabilities, LlmRequest, LlmResponse, LlmStreamEvent,
};
use crate::process::{
    SpawnedChatMessage, SpawnedEnvelope, SpawnedEnvelopeKind, SpawnedMcpServer, SpawnedOperation,
    SpawnedWorker, StreamControl,
};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_LOCAL_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub struct LocalLlmProvider {
    config: LlmProviderConfig,
    worker: SpawnedWorker,
}

impl LocalLlmProvider {
    pub fn new(config: LlmProviderConfig) -> Result<Self> {
        config.validate()?;
        let spawn = config
            .spawn
            .clone()
            .ok_or_else(|| NeuralError::MissingValue {
                value: "spawn".to_string(),
                expected: "local provider spawn config".to_string(),
            })?;
        Ok(Self {
            config,
            worker: SpawnedWorker::new(spawn)?,
        })
    }
}

impl LlmProvider for LocalLlmProvider {
    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        local_provider_capabilities(&self.config.provider_id)
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        validate_llm_request(request)?;
        validate_text_only_modality_inputs(&self.config.provider_id, request)?;
        let response = self
            .worker
            .run_protobuf(self.protobuf_request(request, SpawnedOperation::LlmComplete))?;
        self.llm_response(response)
    }

    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        validate_llm_request(request)?;
        validate_text_only_modality_inputs(&self.config.provider_id, request)?;
        let mut completed = false;
        self.worker.run_protobuf_stream(
            self.protobuf_request(request, SpawnedOperation::LlmStream),
            control.clone(),
            &mut |envelope| {
                let event = llm_stream_event(envelope)?;
                completed = matches!(event, LlmStreamEvent::Complete);
                on_event(event)
            },
        )?;
        maybe_emit_cancelled(control, completed, on_event)
    }
}

impl LocalLlmProvider {
    fn protobuf_request(
        &self,
        request: &LlmRequest,
        operation: SpawnedOperation,
    ) -> SpawnedEnvelope {
        let request_id = NEXT_LOCAL_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let mut envelope = SpawnedEnvelope::request(operation, format!("local-{request_id}"));
        envelope.model = request.model_id().unwrap_or(&self.config.model).to_string();
        envelope.messages = request
            .messages
            .iter()
            .map(|message| SpawnedChatMessage {
                role: message.role.clone(),
                content: message.content.clone(),
            })
            .collect();
        envelope.mcp_servers = request
            .mcp_servers
            .iter()
            .map(|server| SpawnedMcpServer {
                name: server.name.clone(),
                url: server.url.clone(),
            })
            .collect();
        envelope
    }

    fn llm_response(&self, response: SpawnedEnvelope) -> Result<LlmResponse> {
        if response.envelope_kind()? != SpawnedEnvelopeKind::Data {
            return Err(NeuralError::MalformedPayload {
                value: response.kind.to_string(),
                expected: "local LLM data response".into(),
            });
        }
        Ok(LlmResponse {
            provider_id: self.config.provider_id.clone(),
            model: response.model,
            content: response.text,
            metadata: json!({"engine_id": response.engine_id}),
        })
    }
}

pub(crate) fn validate_llm_request(request: &LlmRequest) -> Result<()> {
    if request.messages.is_empty() {
        return Err(NeuralError::InvalidValue {
            value: "empty messages".to_string(),
            expected: "at least one LLM message".to_string(),
        });
    }
    for message in &request.messages {
        require_non_empty(&message.role, "non-empty LLM message role")?;
        require_non_empty(&message.content, "non-empty LLM message content")?;
    }
    if let Some(model) = &request.model {
        require_non_empty(model, "non-empty LLM request model")?;
    }
    if let Some(provider_id) = &request.provider_id {
        require_non_empty(provider_id, "non-empty LLM request provider id")?;
    }
    for server in &request.mcp_servers {
        require_non_empty(&server.name, "non-empty LLM MCP server name")?;
        require_non_empty(&server.url, "non-empty LLM MCP server URL")?;
    }
    for input in &request.modality_inputs {
        require_non_empty(&input.input_id, "non-empty LLM modality input id")?;
        require_non_empty(&input.media_type, "non-empty LLM modality media type")?;
        if input.bytes.is_empty() {
            return Err(NeuralError::InvalidValue {
                value: input.input_id.clone(),
                expected: "non-empty LLM modality bytes".to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn validate_text_only_modality_inputs(
    provider_id: &str,
    request: &LlmRequest,
) -> Result<()> {
    if let Some(input) = request.modality_inputs.first() {
        let expected = match input.kind {
            LlmModalityInputKind::ImageSnapshot => "image_snapshot_input is unsupported",
            LlmModalityInputKind::LiveAudioChunk => "live_audio_input is unsupported",
            LlmModalityInputKind::ScreenFrame => "screen_frame_broadcast_input is unsupported",
        };
        return Err(NeuralError::InvalidValue {
            value: format!("{provider_id}:{}", input.input_id),
            expected: format!("text-only LLM request; {expected}"),
        });
    }
    Ok(())
}

pub(crate) fn llm_stream_event(event: SpawnedEnvelope) -> Result<LlmStreamEvent> {
    match event.envelope_kind()? {
        SpawnedEnvelopeKind::Data => Ok(LlmStreamEvent::ContentDelta { text: event.text }),
        SpawnedEnvelopeKind::Complete => Ok(LlmStreamEvent::Complete),
        SpawnedEnvelopeKind::Cancellation => Ok(LlmStreamEvent::Cancelled),
        SpawnedEnvelopeKind::Failure => Ok(LlmStreamEvent::Error {
            message: event.error_message,
        }),
        kind => Err(NeuralError::MalformedPayload {
            value: format!("{kind:?}"),
            expected: "stream data, completion, cancellation, or failure".into(),
        }),
    }
}

fn maybe_emit_cancelled(
    control: StreamControl,
    completed: bool,
    on_event: &mut LlmStreamEventSink<'_>,
) -> Result<()> {
    if control.max_events.is_some() && !completed {
        on_event(LlmStreamEvent::Cancelled)?;
    }
    Ok(())
}
