use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::capabilities::cerebras_provider_capabilities;
use crate::llm_providers::command_runner::selected_model;
use crate::llm_providers::contract::{
    LlmHttpClient, LlmProvider, LlmStreamEventSink, ProviderCallControl,
};
use crate::llm_providers::openai_compatible::{
    OpenAiCompatibleChatProvider, OpenAiCompatibleProviderSpec,
};
use crate::llm_providers::{
    LlmModalityInputKind, LlmProviderCapabilities, LlmRequest, LlmResponse,
};
use crate::process::StreamControl;
use std::sync::Arc;
use std::time::Instant;

const CEREBRAS_TOOL_CALL_LIMIT: usize = 12;

/// Cerebras models permitted to receive image input. Vision support is
/// model-dependent: any model outside this list must reject image input
/// before HTTP (sprint 2026-07-04 decision; `gemma-4-31b` is the initial entry).
const CEREBRAS_VISION_MODELS: &[&str] = &["gemma-4-31b"];

pub struct CerebrasProvider {
    config: LlmProviderConfig,
    inner: OpenAiCompatibleChatProvider,
}

impl CerebrasProvider {
    /// Creates a Cerebras OpenAI-compatible Chat Completions provider.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind};
    /// # use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
    /// let config = LlmProviderConfig {
    ///     provider_id: "cerebras".into(),
    ///     kind: LlmProviderKind::Cerebras,
    ///     model: "gemma-4-31b".into(),
    ///     endpoint: Some("https://api.cerebras.ai/v1".into()),
    ///     credential: Some("token".into()),
    ///     completion_concurrency: None,
    ///     spawn: None,
    /// };
    /// let _provider = lumvise_neural_core::llm_providers::cerebras::CerebrasProvider::new(
    ///     config,
    ///     Arc::new(ReqwestLlmHttpClient::new()),
    /// );
    /// ```
    pub fn new(config: LlmProviderConfig, http_client: Arc<dyn LlmHttpClient>) -> Result<Self> {
        let inner =
            OpenAiCompatibleChatProvider::new(config.clone(), http_client, cerebras_spec())?;
        Ok(Self { config, inner })
    }

    // Model-dependent image gating. Cerebras only permits image_snapshot_input
    // for allowlisted vision models; everything else fails before any HTTP call.
    fn reject_unsupported_image_model(&self, request: &LlmRequest) -> Result<()> {
        let has_image = request
            .modality_inputs
            .iter()
            .any(|input| matches!(input.kind, LlmModalityInputKind::ImageSnapshot));
        if !has_image {
            return Ok(());
        }
        let model = selected_model(&self.config, request);
        if CEREBRAS_VISION_MODELS.contains(&model.as_str()) {
            return Ok(());
        }
        Err(NeuralError::InvalidValue {
            value: format!("{}:{model}", self.config.provider_id),
            expected: format!(
                "Cerebras vision model from {:?} for image_snapshot_input; got {:?}",
                CEREBRAS_VISION_MODELS, model
            ),
        })
    }
}

impl LlmProvider for CerebrasProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        cerebras_provider_capabilities(self.provider_id())
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        self.reject_unsupported_image_model(request)?;
        self.inner.complete(request)
    }

    fn complete_controlled(
        &self,
        request: &LlmRequest,
        control: &dyn ProviderCallControl,
    ) -> Result<LlmResponse> {
        if control.is_cancelled() {
            return Err(NeuralError::ProcessCancelled {
                command: self.provider_id().into(),
            });
        }
        if control.is_expired() {
            return Err(NeuralError::ProcessTimeout {
                command: self.provider_id().into(),
                timeout_ms: 0,
            });
        }
        self.reject_unsupported_image_model(request)?;
        self.inner.complete_with_control(
            request,
            StreamControl::unbounded().with_deadline(Instant::now() + control.remaining()),
        )
    }

    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        self.reject_unsupported_image_model(request)?;
        self.inner.stream_with_events(request, control, on_event)
    }
}

fn cerebras_spec() -> OpenAiCompatibleProviderSpec {
    OpenAiCompatibleProviderSpec {
        label: "Cerebras",
        supports_image_snapshot: true,
        tool_call_limit: CEREBRAS_TOOL_CALL_LIMIT,
    }
}
