use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::capabilities::openrouter_provider_capabilities;
use crate::llm_providers::contract::{
    LlmHttpClient, LlmProvider, LlmStreamEventSink, ProviderCallControl,
};
use crate::llm_providers::openai_compatible::{
    OpenAiCompatibleChatProvider, OpenAiCompatibleProviderSpec,
};
use crate::llm_providers::{LlmProviderCapabilities, LlmRequest, LlmResponse};
use crate::process::StreamControl;
use std::sync::Arc;
use std::time::Instant;

// Staged voice/canvas explanations alternate narration, reads, and edits.
// Six rounds cut off ordinary explanations; the caller deadline still bounds
// elapsed time independently of this loop guard.
const OPENROUTER_TOOL_CALL_LIMIT: usize = 32;

pub struct OpenRouterProvider {
    inner: OpenAiCompatibleChatProvider,
}

impl OpenRouterProvider {
    /// Creates an OpenRouter Chat Completions provider.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind};
    /// # use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
    /// let config = LlmProviderConfig {
    ///     provider_id: "openrouter".into(),
    ///     kind: LlmProviderKind::OpenRouter,
    ///     model: "openai/gpt-4o-mini".into(),
    ///     endpoint: Some("https://openrouter.ai/api/v1".into()),
    ///     credential: Some("token".into()),
    ///     completion_concurrency: None,
    ///     spawn: None,
    /// };
    /// let _provider = lumvise_neural_core::llm_providers::openrouter::OpenRouterProvider::new(
    ///     config,
    ///     Arc::new(ReqwestLlmHttpClient::new()),
    /// );
    /// ```
    pub fn new(config: LlmProviderConfig, http_client: Arc<dyn LlmHttpClient>) -> Result<Self> {
        Ok(Self {
            inner: OpenAiCompatibleChatProvider::new(config, http_client, openrouter_spec())?,
        })
    }
}

impl LlmProvider for OpenRouterProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        openrouter_provider_capabilities(self.provider_id())
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
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
        self.inner.stream_with_events(request, control, on_event)
    }
}

fn openrouter_spec() -> OpenAiCompatibleProviderSpec {
    OpenAiCompatibleProviderSpec {
        label: "OpenRouter",
        supports_image_snapshot: true,
        tool_call_limit: OPENROUTER_TOOL_CALL_LIMIT,
    }
}
