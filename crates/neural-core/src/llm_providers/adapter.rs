use crate::config::{LlmProviderConfig, LlmProviderKind};
use crate::error::{NeuralError, Result};
use crate::llm_providers::{
    LlmCapabilitySupport, LlmModalityInputKind, LlmProviderCapabilities, LlmRequest,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmTransportPreference {
    Auto,
    PreferClient,
    PreferDirectApi,
    PreferLive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmTransportKind {
    Client,
    DirectApi,
    Live,
    LocalProcess,
}

impl LlmTransportKind {
    pub fn metric_label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::DirectApi => "direct_api",
            Self::Live => "live",
            Self::LocalProcess => "local_process",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmMcpToolMode {
    NativeMcp,
    FunctionToolBridge,
    Passthrough,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmSessionSupport {
    ProviderSessionId,
    LiveResumptionHandle,
    Stateless,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmLatencyClass {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmAdapterPreferences {
    pub transport_preference: LlmTransportPreference,
    pub allow_fallbacks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmNegotiatedCapabilities {
    pub provider_id: String,
    pub selected_transport: LlmTransportKind,
    pub fallback_transports: Vec<LlmTransportKind>,
    pub direct_api_active: bool,
    pub client_transport_active: bool,
    pub live_streaming_active: bool,
    pub final_text_output: bool,
    pub streamed_text_output: bool,
    pub image_snapshot_input: bool,
    pub live_audio_input: bool,
    pub screen_frame_broadcast_input: bool,
    pub native_audio_output: bool,
    pub mcp_tool_mode: LlmMcpToolMode,
    pub session_support: LlmSessionSupport,
    pub latency_class: LlmLatencyClass,
    pub degraded_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSessionRoute {
    provider_id: String,
    transport: LlmTransportKind,
}

impl LlmSessionRoute {
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn transport(&self) -> LlmTransportKind {
        self.transport
    }

    pub(crate) fn new_injected(provider_id: &str, transport: LlmTransportKind) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            transport,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LlmProviderAdapterPlan {
    config: LlmProviderConfig,
    preferences: LlmAdapterPreferences,
}

/// A transport negotiated once for construction and catalog synchronization.
/// Registry construction consumes this value instead of negotiating again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedLlmProviderAdapterPlan {
    pub(crate) plan: LlmProviderAdapterPlan,
    pub(crate) selected_transport: LlmTransportKind,
}

impl Default for LlmAdapterPreferences {
    fn default() -> Self {
        Self {
            transport_preference: LlmTransportPreference::Auto,
            allow_fallbacks: true,
        }
    }
}

impl LlmProviderAdapterPlan {
    pub(crate) fn new(config: LlmProviderConfig, preferences: LlmAdapterPreferences) -> Self {
        Self {
            config,
            preferences,
        }
    }

    pub(crate) fn negotiate(&self, request: &LlmRequest) -> Result<LlmNegotiatedCapabilities> {
        let ranked = self.ranked_transports(request);
        let available = self.available_transports();
        let selected = self.select_transport(&ranked, &available, request)?;
        let reasons = self.degraded_reasons(&ranked, &available, selected);
        Ok(self.capabilities(selected, available, reasons))
    }

    pub(crate) fn selected_transport(&self, request: &LlmRequest) -> Result<LlmTransportKind> {
        Ok(self.negotiate(request)?.selected_transport)
    }

    pub(crate) fn resolve_session_route(&self, request: &LlmRequest) -> Result<LlmSessionRoute> {
        Ok(LlmSessionRoute {
            provider_id: self.config.provider_id.clone(),
            transport: self.selected_transport(request)?,
        })
    }

    pub(crate) fn resolve(self, request: &LlmRequest) -> Result<ResolvedLlmProviderAdapterPlan> {
        let selected_transport = self.selected_transport(request)?;
        Ok(ResolvedLlmProviderAdapterPlan {
            plan: self,
            selected_transport,
        })
    }

    pub(crate) fn configured_model(&self) -> &str {
        &self.config.model
    }

    fn select_transport(
        &self,
        ranked: &[LlmTransportKind],
        available: &[LlmTransportKind],
        request: &LlmRequest,
    ) -> Result<LlmTransportKind> {
        if let Some(transport) = ranked
            .iter()
            .copied()
            .filter(|transport| available.contains(transport))
            .find(|transport| self.transport_supports_request(*transport, request))
        {
            return Ok(transport);
        }
        if let Some(transport) = ranked
            .iter()
            .copied()
            .find(|transport| available.contains(transport))
        {
            self.ensure_transport_supports_request(transport, request)?;
        }
        Err(self.unavailable_error(ranked))
    }

    fn transport_supports_request(
        &self,
        transport: LlmTransportKind,
        request: &LlmRequest,
    ) -> bool {
        request
            .modality_inputs
            .iter()
            .all(|input| self.transport_supports_input(transport, input.kind))
    }

    fn unavailable_error(&self, ranked: &[LlmTransportKind]) -> NeuralError {
        NeuralError::MissingValue {
            value: self.config.provider_id.clone(),
            expected: self.missing_expectation(ranked),
        }
    }

    fn missing_expectation(&self, ranked: &[LlmTransportKind]) -> String {
        ranked
            .iter()
            .map(|transport| self.prerequisite_for(*transport))
            .collect::<Vec<_>>()
            .join(" or ")
    }

    fn degraded_reasons(
        &self,
        ranked: &[LlmTransportKind],
        available: &[LlmTransportKind],
        selected: LlmTransportKind,
    ) -> Vec<String> {
        ranked
            .iter()
            .take_while(|transport| **transport != selected)
            .filter(|transport| !available.contains(transport))
            .map(|transport| self.prerequisite_for(*transport))
            .collect()
    }

    fn ranked_transports(&self, request: &LlmRequest) -> Vec<LlmTransportKind> {
        let mut ranked = self.preferred_transports(request);
        if self.preferences.allow_fallbacks {
            ranked.extend(self.default_transports(request));
        }
        dedupe_transports(ranked)
    }

    fn preferred_transports(&self, request: &LlmRequest) -> Vec<LlmTransportKind> {
        match self.preferences.transport_preference {
            LlmTransportPreference::Auto => self.default_transports(request),
            LlmTransportPreference::PreferClient => vec![self.client_transport()],
            LlmTransportPreference::PreferDirectApi => vec![LlmTransportKind::DirectApi],
            LlmTransportPreference::PreferLive => vec![LlmTransportKind::Live],
        }
    }

    fn default_transports(&self, request: &LlmRequest) -> Vec<LlmTransportKind> {
        if request_requires_live_transport(request) && self.provider_supports_live_transport() {
            return vec![LlmTransportKind::Live, LlmTransportKind::DirectApi];
        }
        match self.config.kind {
            LlmProviderKind::OpenAiRealtime => vec![LlmTransportKind::Live],
            LlmProviderKind::Cerebras
            | LlmProviderKind::OpenRouter
            | LlmProviderKind::Zai
            | LlmProviderKind::OpenAiCompatible => vec![LlmTransportKind::DirectApi],
            LlmProviderKind::Local => vec![LlmTransportKind::LocalProcess],
            LlmProviderKind::Claude | LlmProviderKind::Codex | LlmProviderKind::Gemini => {
                if self.has_credential() {
                    vec![LlmTransportKind::DirectApi, self.client_transport()]
                } else {
                    vec![self.client_transport(), LlmTransportKind::DirectApi]
                }
            }
        }
    }

    fn client_transport(&self) -> LlmTransportKind {
        match self.config.kind {
            LlmProviderKind::Local => LlmTransportKind::LocalProcess,
            _ => LlmTransportKind::Client,
        }
    }

    fn available_transports(&self) -> Vec<LlmTransportKind> {
        let mut transports = Vec::new();
        self.push_available_client(&mut transports);
        self.push_available_direct_api(&mut transports);
        self.push_available_live(&mut transports);
        transports
    }

    fn push_available_client(&self, transports: &mut Vec<LlmTransportKind>) {
        if self.config.spawn.is_none() {
            return;
        }
        match self.config.kind {
            LlmProviderKind::Local => transports.push(LlmTransportKind::LocalProcess),
            LlmProviderKind::Claude | LlmProviderKind::Codex | LlmProviderKind::Gemini => {
                transports.push(LlmTransportKind::Client)
            }
            _ => {}
        }
    }

    fn push_available_direct_api(&self, transports: &mut Vec<LlmTransportKind>) {
        // OpenAI-compatible endpoints (Ollama, vLLM, LM Studio) routinely run
        // without any credential, so their DirectApi transport is available
        // from the endpoint alone.
        if !self.has_credential() && self.config.kind != LlmProviderKind::OpenAiCompatible {
            return;
        }
        match self.config.kind {
            LlmProviderKind::Cerebras
            | LlmProviderKind::Claude
            | LlmProviderKind::Codex
            | LlmProviderKind::Gemini
            | LlmProviderKind::OpenAiCompatible
            | LlmProviderKind::OpenRouter
            | LlmProviderKind::Zai => transports.push(LlmTransportKind::DirectApi),
            _ => {}
        }
    }

    fn push_available_live(&self, transports: &mut Vec<LlmTransportKind>) {
        if !self.has_credential() {
            return;
        }
        match self.config.kind {
            LlmProviderKind::Gemini | LlmProviderKind::OpenAiRealtime => {
                transports.push(LlmTransportKind::Live)
            }
            _ => {}
        }
    }

    fn provider_supports_live_transport(&self) -> bool {
        matches!(
            self.config.kind,
            LlmProviderKind::Gemini | LlmProviderKind::OpenAiRealtime
        )
    }

    fn has_credential(&self) -> bool {
        self.config
            .credential
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    }

    fn prerequisite_for(&self, transport: LlmTransportKind) -> String {
        match transport {
            LlmTransportKind::Client => "client spawn config".to_string(),
            LlmTransportKind::DirectApi => "direct API credential".to_string(),
            LlmTransportKind::Live => "live API credential".to_string(),
            LlmTransportKind::LocalProcess => "local provider spawn config".to_string(),
        }
    }

    fn ensure_transport_supports_request(
        &self,
        transport: LlmTransportKind,
        request: &LlmRequest,
    ) -> Result<()> {
        for input in &request.modality_inputs {
            self.ensure_transport_supports_input(transport, input.kind, &input.input_id)?;
        }
        Ok(())
    }

    fn ensure_transport_supports_input(
        &self,
        transport: LlmTransportKind,
        kind: LlmModalityInputKind,
        input_id: &str,
    ) -> Result<()> {
        if self.transport_supports_input(transport, kind) {
            return Ok(());
        }
        Err(NeuralError::InvalidValue {
            value: format!("{}:{input_id}", self.config.provider_id),
            expected: unsupported_input_expectation(kind),
        })
    }

    fn transport_supports_input(
        &self,
        transport: LlmTransportKind,
        kind: LlmModalityInputKind,
    ) -> bool {
        match kind {
            LlmModalityInputKind::ImageSnapshot => self.supports_image_snapshot(transport),
            LlmModalityInputKind::LiveAudioChunk => transport == LlmTransportKind::Live,
            LlmModalityInputKind::ScreenFrame => transport == LlmTransportKind::Live,
        }
    }

    fn supports_image_snapshot(&self, transport: LlmTransportKind) -> bool {
        match transport {
            LlmTransportKind::DirectApi => !matches!(
                self.config.kind,
                LlmProviderKind::Local | LlmProviderKind::OpenAiCompatible | LlmProviderKind::Zai
            ),
            LlmTransportKind::Live => self.config.kind == LlmProviderKind::OpenAiRealtime,
            LlmTransportKind::Client => self.config.kind == LlmProviderKind::Claude,
            LlmTransportKind::LocalProcess => false,
        }
    }

    fn capabilities(
        &self,
        selected: LlmTransportKind,
        available: Vec<LlmTransportKind>,
        degraded_reasons: Vec<String>,
    ) -> LlmNegotiatedCapabilities {
        LlmNegotiatedCapabilities {
            provider_id: self.config.provider_id.clone(),
            selected_transport: selected,
            fallback_transports: fallback_transports(&available, selected),
            direct_api_active: selected == LlmTransportKind::DirectApi,
            client_transport_active: selected == LlmTransportKind::Client,
            live_streaming_active: selected == LlmTransportKind::Live,
            final_text_output: true,
            streamed_text_output: true,
            image_snapshot_input: self.supports_image_snapshot(selected),
            live_audio_input: selected == LlmTransportKind::Live,
            screen_frame_broadcast_input: selected == LlmTransportKind::Live,
            native_audio_output: false,
            mcp_tool_mode: self.mcp_tool_mode(selected),
            session_support: self.session_support(selected),
            latency_class: latency_class(selected),
            degraded_reasons,
        }
    }

    fn mcp_tool_mode(&self, transport: LlmTransportKind) -> LlmMcpToolMode {
        match transport {
            LlmTransportKind::Client => LlmMcpToolMode::NativeMcp,
            LlmTransportKind::DirectApi | LlmTransportKind::Live => {
                LlmMcpToolMode::FunctionToolBridge
            }
            LlmTransportKind::LocalProcess => LlmMcpToolMode::Passthrough,
        }
    }

    fn session_support(&self, transport: LlmTransportKind) -> LlmSessionSupport {
        match (transport, self.config.kind) {
            (LlmTransportKind::Live, _) => LlmSessionSupport::LiveResumptionHandle,
            (LlmTransportKind::Client, _) => LlmSessionSupport::ProviderSessionId,
            (LlmTransportKind::DirectApi, LlmProviderKind::Codex) => {
                LlmSessionSupport::ProviderSessionId
            }
            (LlmTransportKind::DirectApi, _) => LlmSessionSupport::Stateless,
            (LlmTransportKind::LocalProcess, _) => LlmSessionSupport::Unsupported,
        }
    }
}

pub(crate) fn negotiate_injected_provider(
    capabilities: &LlmProviderCapabilities,
    request: &LlmRequest,
) -> Result<LlmNegotiatedCapabilities> {
    validate_injected_inputs(capabilities, request)?;
    let live = request_requires_live_transport(request);
    Ok(LlmNegotiatedCapabilities {
        provider_id: capabilities.provider_id.clone(),
        selected_transport: if live {
            LlmTransportKind::Live
        } else {
            LlmTransportKind::DirectApi
        },
        fallback_transports: Vec::new(),
        direct_api_active: !live,
        client_transport_active: false,
        live_streaming_active: live,
        final_text_output: is_available(capabilities.final_text_output),
        streamed_text_output: is_available(capabilities.streamed_text_output),
        image_snapshot_input: is_available(capabilities.image_snapshot_input),
        live_audio_input: is_available(capabilities.live_audio_input),
        screen_frame_broadcast_input: is_available(capabilities.screen_frame_broadcast_input),
        native_audio_output: is_available(capabilities.native_audio_output),
        mcp_tool_mode: LlmMcpToolMode::FunctionToolBridge,
        session_support: if live {
            LlmSessionSupport::LiveResumptionHandle
        } else {
            LlmSessionSupport::Stateless
        },
        latency_class: if live {
            LlmLatencyClass::Low
        } else {
            LlmLatencyClass::Medium
        },
        degraded_reasons: Vec::new(),
    })
}

fn validate_injected_inputs(
    capabilities: &LlmProviderCapabilities,
    request: &LlmRequest,
) -> Result<()> {
    for input in &request.modality_inputs {
        let support = match input.kind {
            LlmModalityInputKind::ImageSnapshot => capabilities.image_snapshot_input,
            LlmModalityInputKind::LiveAudioChunk => capabilities.live_audio_input,
            LlmModalityInputKind::ScreenFrame => capabilities.screen_frame_broadcast_input,
        };
        if support == LlmCapabilitySupport::Unsupported {
            return Err(NeuralError::InvalidValue {
                value: format!("{}:{}", capabilities.provider_id, input.input_id),
                expected: unsupported_input_expectation(input.kind),
            });
        }
    }
    Ok(())
}

fn is_available(support: LlmCapabilitySupport) -> bool {
    support != LlmCapabilitySupport::Unsupported
}

fn fallback_transports(
    available: &[LlmTransportKind],
    selected: LlmTransportKind,
) -> Vec<LlmTransportKind> {
    available
        .iter()
        .copied()
        .filter(|transport| *transport != selected)
        .collect()
}

fn latency_class(transport: LlmTransportKind) -> LlmLatencyClass {
    match transport {
        LlmTransportKind::Live => LlmLatencyClass::Low,
        LlmTransportKind::DirectApi => LlmLatencyClass::Medium,
        LlmTransportKind::Client | LlmTransportKind::LocalProcess => LlmLatencyClass::High,
    }
}

fn request_requires_live_transport(request: &LlmRequest) -> bool {
    request.modality_inputs.iter().any(|input| {
        matches!(
            input.kind,
            LlmModalityInputKind::LiveAudioChunk | LlmModalityInputKind::ScreenFrame
        )
    })
}

fn unsupported_input_expectation(kind: LlmModalityInputKind) -> String {
    match kind {
        LlmModalityInputKind::ImageSnapshot => "image_snapshot_input is unsupported",
        LlmModalityInputKind::LiveAudioChunk => "live_audio_input is unsupported",
        LlmModalityInputKind::ScreenFrame => "screen_frame_broadcast_input is unsupported",
    }
    .to_string()
}

fn dedupe_transports(transports: Vec<LlmTransportKind>) -> Vec<LlmTransportKind> {
    let mut deduped = Vec::new();
    for transport in transports {
        if !deduped.contains(&transport) {
            deduped.push(transport);
        }
    }
    deduped
}
