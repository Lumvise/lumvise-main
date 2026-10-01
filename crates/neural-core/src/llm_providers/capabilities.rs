use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmCapabilitySupport {
    Supported,
    ModelDependent,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProviderCapabilities {
    pub provider_id: String,
    pub final_text_output: LlmCapabilitySupport,
    pub streamed_text_output: LlmCapabilitySupport,
    pub image_snapshot_input: LlmCapabilitySupport,
    pub live_audio_input: LlmCapabilitySupport,
    pub screen_frame_broadcast_input: LlmCapabilitySupport,
    pub native_audio_output: LlmCapabilitySupport,
}

pub(crate) fn claude_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::ModelDependent,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

pub(crate) fn codex_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

pub(crate) fn gemini_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    let mut capabilities = provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Supported,
    );
    capabilities.native_audio_output = LlmCapabilitySupport::ModelDependent;
    capabilities
}

pub(crate) fn local_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

pub(crate) fn openai_realtime_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    let mut capabilities = provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Supported,
    );
    capabilities.native_audio_output = LlmCapabilitySupport::ModelDependent;
    capabilities
}

pub(crate) fn cerebras_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::ModelDependent,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

pub(crate) fn openrouter_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::ModelDependent,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

pub(crate) fn z_ai_provider_capabilities(provider_id: &str) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

// Text-only like the other direct-API providers: the user-owned endpoint
// model inventory is unknown ahead of time, so image/audio input stays
// unsupported regardless of what the endpoint serves.
pub(crate) fn openai_compatible_provider_capabilities(
    provider_id: &str,
) -> LlmProviderCapabilities {
    provider_capabilities(
        provider_id,
        LlmCapabilitySupport::Supported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
        LlmCapabilitySupport::Unsupported,
    )
}

fn provider_capabilities(
    provider_id: &str,
    streamed_text_output: LlmCapabilitySupport,
    image_snapshot_input: LlmCapabilitySupport,
    live_audio_input: LlmCapabilitySupport,
    screen_frame_broadcast_input: LlmCapabilitySupport,
) -> LlmProviderCapabilities {
    LlmProviderCapabilities {
        provider_id: provider_id.to_string(),
        final_text_output: LlmCapabilitySupport::Supported,
        streamed_text_output,
        image_snapshot_input,
        live_audio_input,
        screen_frame_broadcast_input,
        native_audio_output: LlmCapabilitySupport::Unsupported,
    }
}
