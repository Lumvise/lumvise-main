//! Owns neutral LLM requests, configured selection and execution routing.
//! Host adapters call the request methods and selection entrypoints; provider details stay here.
use super::*;

pub(super) const MAX_LLM_MESSAGES: usize = 128;
pub(super) const MAX_LLM_SERVERS: usize = 16;
pub(super) const MAX_LLM_TEXT_BYTES: usize = 256 * 1024;
pub(super) const MAX_LLM_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

impl PluginHostServices {
    /// Whether the app has an Assistant Engine selected and active — reuses
    /// the exact background-job provider/model resolution so the two never
    /// drift (issue #49).
    pub(super) fn assistant_engine_availability(&self) -> Result<Value, HostCapabilityError> {
        let (provider, model) =
            configured_llm_selection(&self.frontend, ASSISTANT_ENGINE_AVAILABILITY)?;
        // Lumvise Cloud chooses its model on the website; its saved model is intentionally empty.
        let configured = provider.as_deref() == Some(AssistantEngine::LumviseCloud.value())
            || (provider.is_some() && model.is_some());
        Ok(json!({"available": configured}))
    }

    pub(super) fn neural_llm(
        &self,
        plugin_id: &str,
        input: Value,
        control: LlmExecutionControl,
    ) -> Result<Value, HostCapabilityError> {
        let mut request: NeutralLlmRequest = decode(NEURAL_LLM, input)?;
        request.validate()?;
        let provider_id = resolve_llm_selection(&self.frontend, NEURAL_LLM, &mut request)?;
        let ticket = self.activity.track_llm(plugin_id, &provider_id);
        let result = selected_llm_executor(
            plugin_id,
            request.conversation_id.as_deref(),
            &self.llm_executors,
            &self.assistant_llm_executors,
            &self.workspace_llm_executors,
        )
        .complete(&provider_id, request.into_neural(), ticket.control(control));
        ticket.finish(&result);
        let response = result.map_err(llm_execution_error)?;
        bounded_output(
            NEURAL_LLM,
            json!({"response": response}),
            MAX_LLM_OUTPUT_BYTES,
        )
    }
}

pub(super) fn selected_llm_executor<'a>(
    plugin_id: &str,
    conversation_id: Option<&str>,
    ordinary: &'a Arc<LlmExecutorRegistry>,
    assistant: &'a Arc<LlmExecutorRegistry>,
    workspace: &'a Arc<LlmExecutorRegistry>,
) -> &'a Arc<LlmExecutorRegistry> {
    if plugin_id == "builtin.assistant" && conversation_id.is_some_and(workspace_session) {
        workspace
    } else if plugin_id == "builtin.assistant" {
        assistant
    } else {
        ordinary
    }
}

fn llm_execution_error(error: LlmFailure) -> HostCapabilityError {
    failed(NEURAL_LLM, &error.message)
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NeutralLlmRequest {
    #[serde(default)]
    pub(super) provider_id: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) conversation_id: Option<String>,
    #[serde(default)]
    pub(super) llm_session_id: Option<String>,
    pub(super) messages: Vec<LlmMessage>,
    pub(super) mcp_servers: Vec<LlmMcpServerConfig>,
    #[serde(default)]
    pub(super) options: LlmRequestOptions,
}

impl NeutralLlmRequest {
    pub(super) fn validate(&self) -> Result<(), HostCapabilityError> {
        if self.messages.len() > MAX_LLM_MESSAGES {
            return Err(quota(NEURAL_LLM, self.messages.len(), MAX_LLM_MESSAGES));
        }
        if self.mcp_servers.len() > MAX_LLM_SERVERS {
            return Err(quota(NEURAL_LLM, self.mcp_servers.len(), MAX_LLM_SERVERS));
        }
        let bytes = self
            .messages
            .iter()
            .map(|message| message.role.len() + message.content.len())
            .sum::<usize>();
        if bytes > MAX_LLM_TEXT_BYTES {
            return Err(quota(NEURAL_LLM, bytes, MAX_LLM_TEXT_BYTES));
        }
        for message in &self.messages {
            require_non_empty(NEURAL_LLM, &message.role, "message role")?;
            require_non_empty(NEURAL_LLM, &message.content, "message content")?;
        }
        if let Some(temperature) = self.options.temperature {
            if !(0.0..=2.0).contains(&temperature) {
                return Err(invalid(
                    NEURAL_LLM,
                    &json!(temperature),
                    "temperature within 0.0 through 2.0",
                ));
            }
        }
        if let Some(tokens) = self.options.max_output_tokens {
            if tokens == 0 || tokens > 32768 {
                return Err(invalid(
                    NEURAL_LLM,
                    &json!(tokens),
                    "max_output_tokens from 1 through 32768",
                ));
            }
        }
        if let Some(LlmResponseFormat::JsonSchema { name, schema, .. }) =
            self.options.response_format.as_ref()
        {
            if name.trim().is_empty() || !schema.is_object() {
                return Err(invalid(
                    NEURAL_LLM,
                    &json!(self.options.response_format),
                    "json_schema format with non-empty name and object schema",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn into_neural(self) -> LlmRequest {
        LlmRequest {
            options: self.options,
            messages: self.messages,
            stream: self.provider_id.as_deref() == Some(AssistantEngine::LumviseCloud.value()),
            provider_id: self.provider_id,
            model: self.model,
            conversation_id: self.conversation_id,
            provider_session_id: self.llm_session_id,
            mcp_servers: self.mcp_servers,
            modality_inputs: Vec::new(),
        }
    }
}

pub(super) fn resolve_llm_selection(
    frontend: &Mutex<FrontendCore>,
    capability: &str,
    request: &mut NeutralLlmRequest,
) -> Result<String, HostCapabilityError> {
    apply_configured_llm_selection(frontend, capability, request)?
        .ok_or_else(|| unavailable(capability, "no provider_id was selected"))
}
/// Fills `provider_id`/`model` from the app's configured assistant engine
/// when the caller left them unset. Returns `None` (never errors on a
/// missing provider) when neither an explicit nor a configured provider is
/// available, so callers that can defer resolution to later execution
/// (`runtime.background_job` queues a job before the provider ever runs)
/// accept an unresolved request as-is; `resolve_llm_selection` above layers
/// the immediate hard error synchronous callers (`neural_llm`) need.
pub(super) fn apply_configured_llm_selection(
    frontend: &Mutex<FrontendCore>,
    capability: &str,
    request: &mut NeutralLlmRequest,
) -> Result<Option<String>, HostCapabilityError> {
    let explicit_provider = non_empty_option(request.provider_id.as_deref()).map(str::to_owned);
    let (configured_provider, configured_model) = configured_llm_selection(frontend, capability)?;
    let provider_id = explicit_provider.clone().or(configured_provider.clone());
    request.provider_id = provider_id.clone();
    if non_empty_option(request.model.as_deref()).is_none()
        && explicit_provider
            .as_ref()
            .is_none_or(|selected| Some(selected) == configured_provider.as_ref())
    {
        request.model = configured_model;
    }
    Ok(provider_id)
}
pub(super) fn configured_llm_selection(
    frontend: &Mutex<FrontendCore>,
    capability: &str,
) -> Result<(Option<String>, Option<String>), HostCapabilityError> {
    let frontend = frontend
        .lock()
        .map_err(|_| failed(capability, "frontend mutex poisoned"))?;
    let settings = frontend.app_settings();
    let provider = (settings.assistant_engine != AssistantEngine::NativeMcp)
        .then(|| settings.assistant_engine.value().to_owned());
    let model = non_empty_option(settings.assistant_model.as_deref()).map(str::to_owned);
    Ok((provider, model))
}

pub(super) fn non_empty_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
