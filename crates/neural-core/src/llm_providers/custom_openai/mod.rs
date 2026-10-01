use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::capabilities::openai_compatible_provider_capabilities;
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

const CUSTOM_OPENAI_TOOL_CALL_LIMIT: usize = 6;

pub struct CustomOpenAiProvider {
    inner: OpenAiCompatibleChatProvider,
}

impl CustomOpenAiProvider {
    /// Creates a user-owned OpenAI-compatible Chat Completions provider
    /// (Ollama, vLLM, LM Studio, Azure/OpenAI, company gateways).
    ///
    /// The credential is optional: local servers frequently require none, and
    /// [`ReqwestLlmHttpClient`] omits the Authorization header when it is
    /// empty.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind};
    /// # use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
    /// let config = LlmProviderConfig {
    ///     provider_id: "custom_openai".into(),
    ///     kind: LlmProviderKind::OpenAiCompatible,
    ///     model: "provider-default".into(),
    ///     endpoint: Some("http://localhost:11434/v1".into()),
    ///     credential: None,
    ///     completion_concurrency: None,
    ///     spawn: None,
    /// };
    /// let _provider =
    ///     lumvise_neural_core::llm_providers::custom_openai::CustomOpenAiProvider::new(
    ///         config,
    ///         Arc::new(ReqwestLlmHttpClient::new()),
    ///     );
    /// ```
    pub fn new(config: LlmProviderConfig, http_client: Arc<dyn LlmHttpClient>) -> Result<Self> {
        Ok(Self {
            inner: OpenAiCompatibleChatProvider::new(config, http_client, custom_openai_spec())?,
        })
    }
}

impl LlmProvider for CustomOpenAiProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        openai_compatible_provider_capabilities(self.provider_id())
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

fn custom_openai_spec() -> OpenAiCompatibleProviderSpec {
    OpenAiCompatibleProviderSpec {
        label: "OpenAI-compatible",
        supports_image_snapshot: false,
        tool_call_limit: CUSTOM_OPENAI_TOOL_CALL_LIMIT,
    }
}
