use crate::config::{LlmProviderConfig, LlmProviderKind};
use crate::error::{NeuralError, Result, require_non_empty};
use crate::llm_providers::adapter::{
    LlmAdapterPreferences, LlmNegotiatedCapabilities, LlmProviderAdapterPlan, LlmSessionRoute,
    LlmTransportKind, ResolvedLlmProviderAdapterPlan, negotiate_injected_provider,
};
use crate::llm_providers::contract::{LlmHttpClient, LlmProvider, LlmStreamEventSink};
use crate::llm_providers::local::LocalLlmProvider;
use crate::llm_providers::model_catalog::{LlmModelSource, ProviderModelSources};
use crate::llm_providers::{LlmProviderCapabilities, LlmRequest, LlmResponse, LlmStreamEvent};
use crate::process::StreamControl;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct LlmProviderRegistry {
    providers: BTreeMap<String, Arc<dyn LlmProvider>>,
    adapter_plans: BTreeMap<String, LlmProviderAdapterPlan>,
    model_sources: BTreeMap<String, ProviderModelSources>,
    completion_concurrency: BTreeMap<String, usize>,
}

pub(crate) struct ResolvedProviderConfig {
    pub(crate) config: LlmProviderConfig,
    pub(crate) resolved_plan: ResolvedLlmProviderAdapterPlan,
    pub(crate) model_sources: ProviderModelSources,
}
impl LlmProviderRegistry {
    /// Builds a registry from provider configs and an HTTP client adapter.
    ///
    /// # Example
    ///
    /// ```
    /// let registry = lumvise_neural_core::LlmProviderRegistry::empty();
    /// assert!(registry.provider_ids().is_empty());
    /// ```
    pub fn empty() -> Self {
        Self {
            providers: BTreeMap::new(),
            adapter_plans: BTreeMap::new(),
            model_sources: BTreeMap::new(),
            completion_concurrency: BTreeMap::new(),
        }
    }

    pub fn from_configs(
        configs: Vec<LlmProviderConfig>,
        http_client: Arc<dyn LlmHttpClient>,
    ) -> Result<Self> {
        Self::from_configs_with_preferences(configs, http_client, LlmAdapterPreferences::default())
    }

    pub fn from_configs_with_preferences(
        configs: Vec<LlmProviderConfig>,
        http_client: Arc<dyn LlmHttpClient>,
        preferences: LlmAdapterPreferences,
    ) -> Result<Self> {
        let mut providers: BTreeMap<String, Arc<dyn LlmProvider>> = BTreeMap::new();
        let mut adapter_plans: BTreeMap<String, LlmProviderAdapterPlan> = BTreeMap::new();
        let mut completion_concurrency: BTreeMap<String, usize> = BTreeMap::new();
        for config in configs {
            config.validate()?;
            let id = config.provider_id.clone();
            let concurrency = config
                .completion_concurrency
                .unwrap_or_else(|| default_completion_concurrency(config.kind));
            let plan = LlmProviderAdapterPlan::new(config.clone(), preferences.clone());
            let provider = provider_from_config(config, http_client.clone(), &plan)?;
            providers.insert(id.clone(), Arc::from(provider));
            adapter_plans.insert(id.clone(), plan);
            completion_concurrency.insert(id, concurrency);
        }
        Ok(Self {
            providers,
            model_sources: BTreeMap::new(),
            adapter_plans,
            completion_concurrency,
        })
    }

    /// Builds synchronized providers from the exact transport used for source
    /// resolution. This prevents construction from independently renegotiating.
    pub(crate) fn from_resolved_configs(
        configs: Vec<ResolvedProviderConfig>,
        http_client: Arc<dyn LlmHttpClient>,
    ) -> Result<Self> {
        let mut providers = BTreeMap::new();
        let mut adapter_plans = BTreeMap::new();
        let mut model_sources = BTreeMap::new();
        let mut completion_concurrency = BTreeMap::new();
        for resolved in configs {
            resolved.config.validate()?;
            let id = resolved.config.provider_id.clone();
            let concurrency = resolved
                .config
                .completion_concurrency
                .unwrap_or_else(|| default_completion_concurrency(resolved.config.kind));
            let provider = provider_from_config_with_transport(
                resolved.config,
                http_client.clone(),
                resolved.resolved_plan.selected_transport,
            )?;
            adapter_plans.insert(id.clone(), resolved.resolved_plan.plan);
            model_sources.insert(id.clone(), resolved.model_sources);
            providers.insert(id.clone(), Arc::from(provider));
            completion_concurrency.insert(id, concurrency);
        }
        Ok(Self {
            providers,
            adapter_plans,
            model_sources,
            completion_concurrency,
        })
    }

    /// Builds a registry from provider instances with owned transport clients.
    ///
    /// # Example
    ///
    /// ```
    /// let registry = lumvise_neural_core::LlmProviderRegistry::from_provider_instances(vec![]);
    /// assert!(registry.is_ok());
    /// ```
    pub fn from_provider_instances(instances: Vec<Box<dyn LlmProvider>>) -> Result<Self> {
        let mut providers: BTreeMap<String, Arc<dyn LlmProvider>> = BTreeMap::new();
        let mut completion_concurrency: BTreeMap<String, usize> = BTreeMap::new();
        for provider in instances {
            let id = provider.provider_id().to_string();
            require_non_empty(&id, "non-empty provider id")?;
            if providers.contains_key(&id) {
                return Err(NeuralError::InvalidValue {
                    value: id,
                    expected: "unique provider id".to_string(),
                });
            }
            providers.insert(id.clone(), Arc::from(provider));
            completion_concurrency.insert(id, 1);
        }
        Ok(Self {
            providers,
            adapter_plans: BTreeMap::new(),
            model_sources: BTreeMap::new(),
            completion_concurrency,
        })
    }

    /// Overrides the per-provider completion concurrency for an already-registered
    /// provider. Lets callers tune injected instances (config-driven concurrency
    /// is otherwise resolved at `from_configs` time from the provider kind).
    pub fn with_completion_concurrency(mut self, provider_id: &str, concurrency: usize) -> Self {
        if self.providers.contains_key(provider_id) {
            self.completion_concurrency
                .insert(provider_id.to_string(), concurrency.max(1));
        }
        self
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn capabilities(&self, provider_id: &str) -> Result<LlmProviderCapabilities> {
        Ok(self.provider(provider_id)?.capabilities())
    }

    pub fn negotiate(
        &self,
        provider_id: &str,
        request: &LlmRequest,
    ) -> Result<LlmNegotiatedCapabilities> {
        match self.adapter_plans.get(provider_id) {
            Some(plan) => {
                let negotiated = plan.negotiate(request)?;
                self.validate_request_model(
                    provider_id,
                    plan,
                    request,
                    negotiated.selected_transport,
                )?;
                Ok(negotiated)
            }
            None => {
                negotiate_injected_provider(&self.provider(provider_id)?.capabilities(), request)
            }
        }
    }
    /// Resolves the transport for one assistant session. Callers retain this
    /// immutable route and reuse it for every retry in that session.
    pub fn resolve_session_route(
        &self,
        provider_id: &str,
        request: &LlmRequest,
    ) -> Result<LlmSessionRoute> {
        match self.adapter_plans.get(provider_id) {
            Some(plan) => {
                let negotiated = plan.negotiate(request)?;
                self.validate_request_model(
                    provider_id,
                    plan,
                    request,
                    negotiated.selected_transport,
                )?;
                plan.resolve_session_route(request)
            }
            None => {
                let provider = self.provider(provider_id)?;
                let negotiated = negotiate_injected_provider(&provider.capabilities(), request)?;
                Ok(LlmSessionRoute::new_injected(
                    provider_id,
                    negotiated.selected_transport,
                ))
            }
        }
    }

    pub fn provider_capabilities(&self) -> Vec<LlmProviderCapabilities> {
        self.providers
            .values()
            .map(|provider| provider.capabilities())
            .collect()
    }

    pub fn complete(&self, provider_id: &str, request: &LlmRequest) -> Result<LlmResponse> {
        self.negotiate(provider_id, request)?;
        self.provider(provider_id)?.complete(request)
    }

    /// Returns a cloneable provider handle so callers release registry synchronization before I/O.
    pub fn provider_handle(&self, provider_id: &str) -> Result<Arc<dyn LlmProvider>> {
        require_non_empty(provider_id, "non-empty provider id")?;
        self.providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| NeuralError::InvalidValue {
                value: provider_id.to_string(),
                expected: format!("configured provider id in {:?}", self.provider_ids()),
            })
    }

    /// Completes through the provider selected in the request.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let response = registry.complete_request(&request)?;
    /// ```
    pub fn complete_request(&self, request: &LlmRequest) -> Result<LlmResponse> {
        let provider_id = self.request_provider_id(request)?;
        self.negotiate(provider_id, request)?;
        self.provider(provider_id)?.complete(request)
    }

    pub fn stream(
        &self,
        provider_id: &str,
        request: &LlmRequest,
        control: StreamControl,
    ) -> Result<Vec<LlmStreamEvent>> {
        self.negotiate(provider_id, request)?;
        self.provider(provider_id)?.stream(request, control)
    }

    pub fn stream_request(
        &self,
        request: &LlmRequest,
        control: StreamControl,
    ) -> Result<Vec<LlmStreamEvent>> {
        let provider_id = self.request_provider_id(request)?;
        self.negotiate(provider_id, request)?;
        self.provider(provider_id)?.stream(request, control)
    }

    pub fn stream_with_events(
        &self,
        provider_id: &str,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        self.negotiate(provider_id, request)?;
        self.provider(provider_id)?
            .stream_with_events(request, control, on_event)
    }

    /// Returns the completion concurrency configured for one provider.
    pub fn completion_concurrency(&self, provider_id: &str) -> usize {
        self.completion_concurrency
            .get(provider_id)
            .copied()
            .unwrap_or(1)
    }

    fn request_provider_id<'a>(&self, request: &'a LlmRequest) -> Result<&'a str> {
        request
            .provider_id
            .as_deref()
            .filter(|provider_id| !provider_id.trim().is_empty())
            .ok_or_else(|| NeuralError::MissingValue {
                value: "request provider_id".to_string(),
                expected: "provider_id on LLM request or explicit registry provider id".to_string(),
            })
    }

    fn validate_request_model(
        &self,
        provider_id: &str,
        plan: &LlmProviderAdapterPlan,
        request: &LlmRequest,
        transport: LlmTransportKind,
    ) -> Result<()> {
        let Some(sources) = self.model_sources.get(provider_id) else {
            return Ok(());
        };
        let source = LlmModelSource::for_transport(transport);
        let inventory = sources
            .for_source(source)
            .ok_or_else(|| NeuralError::InvalidValue {
                value: format!("{provider_id}:{source:?}"),
                expected: "catalog model source compatible with selected transport".to_string(),
            })?;
        let model = request
            .model
            .as_deref()
            .unwrap_or_else(|| plan.configured_model());
        if model != "provider-default"
            && !inventory
                .models
                .iter()
                .any(|candidate| candidate.id == model)
        {
            return Err(NeuralError::InvalidValue {
                value: format!("{provider_id}:{model}"),
                expected: format!("model declared by the active {source:?} catalog source"),
            });
        }
        Ok(())
    }

    fn provider(&self, provider_id: &str) -> Result<&dyn LlmProvider> {
        require_non_empty(provider_id, "non-empty provider id")?;
        self.providers
            .get(provider_id)
            .map(AsRef::as_ref)
            .ok_or_else(|| NeuralError::InvalidValue {
                value: provider_id.to_string(),
                expected: format!("configured provider id in {:?}", self.provider_ids()),
            })
    }
}

const HTTP_COMPLETION_CONCURRENCY: usize = 4;

fn default_completion_concurrency(kind: LlmProviderKind) -> usize {
    match kind {
        LlmProviderKind::Cerebras
        | LlmProviderKind::OpenAiCompatible
        | LlmProviderKind::OpenRouter
        | LlmProviderKind::Zai => HTTP_COMPLETION_CONCURRENCY,
        // App-server conversations own separate processes and session locks.
        // Keep one explanation from blocking the workspace's interactive turn.
        LlmProviderKind::Codex => 2,
        LlmProviderKind::Claude
        | LlmProviderKind::Gemini
        | LlmProviderKind::Local
        | LlmProviderKind::OpenAiRealtime => 1,
    }
}

fn provider_from_config(
    config: LlmProviderConfig,
    http_client: Arc<dyn LlmHttpClient>,
    plan: &LlmProviderAdapterPlan,
) -> Result<Box<dyn LlmProvider>> {
    let selected_transport = plan.selected_transport(&adapter_probe_request(&config))?;
    provider_from_config_with_transport(config, http_client, selected_transport)
}

fn provider_from_config_with_transport(
    config: LlmProviderConfig,
    http_client: Arc<dyn LlmHttpClient>,
    selected_transport: LlmTransportKind,
) -> Result<Box<dyn LlmProvider>> {
    match config.kind {
        LlmProviderKind::Claude => Ok(Box::new(
            crate::llm_providers::claude::ClaudeProvider::new_with_transport(
                config,
                http_client,
                selected_transport,
            )?,
        )),
        LlmProviderKind::Codex => Ok(Box::new(
            crate::llm_providers::codex::CodexProvider::new_with_transport(
                config,
                http_client,
                selected_transport,
            )?,
        )),
        LlmProviderKind::Gemini => Ok(Box::new(
            crate::llm_providers::gemini::GeminiProvider::new_with_transport(
                config,
                http_client,
                selected_transport,
            )?,
        )),
        LlmProviderKind::OpenAiRealtime => Ok(Box::new(
            crate::llm_providers::codex::live::OpenAiRealtimeProvider::new(config)?,
        )),
        LlmProviderKind::Cerebras => Ok(Box::new(
            crate::llm_providers::cerebras::CerebrasProvider::new(config, http_client)?,
        )),
        LlmProviderKind::OpenAiCompatible => Ok(Box::new(
            crate::llm_providers::custom_openai::CustomOpenAiProvider::new(config, http_client)?,
        )),
        LlmProviderKind::OpenRouter => Ok(Box::new(
            crate::llm_providers::openrouter::OpenRouterProvider::new(config, http_client)?,
        )),
        LlmProviderKind::Zai => Ok(Box::new(crate::llm_providers::z_ai::ZAiProvider::new(
            config,
            http_client,
        )?)),
        LlmProviderKind::Local => Ok(Box::new(LocalLlmProvider::new(config)?)),
    }
}

fn adapter_probe_request(config: &LlmProviderConfig) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![crate::llm_providers::LlmMessage {
            role: "user".to_string(),
            content: "adapter probe".to_string(),
        }],
        stream: false,
        provider_id: Some(config.provider_id.clone()),
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}
