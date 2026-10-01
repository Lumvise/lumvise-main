use lumvise_neural_core::llm_providers::capabilities::{
    LlmCapabilitySupport, LlmProviderCapabilities,
};
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::Value;
use std::sync::Arc;

struct EmptyCapabilityHttpClient;

impl LlmHttpClient for EmptyCapabilityHttpClient {
    fn post_json(&self, _request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!("capability lookup must not call remote completion")
    }

    fn stream_text(&self, _request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!("capability lookup must not open a remote stream")
    }
}

#[test]
fn registry_reports_capabilities_for_each_provider_kind() {
    let registry = capability_registry();

    let capabilities = registry.provider_capabilities();

    assert_eq!(capabilities.len(), 8);
    assert_provider_can_stream_text(&capabilities, "cerebras");
    assert_provider_can_stream_text(&capabilities, "claude");
    assert_provider_can_stream_text(&capabilities, "codex");
    assert_provider_can_stream_text(&capabilities, "gemini");
    assert_provider_can_stream_text(&capabilities, "local");
    assert_provider_can_stream_text(&capabilities, "openai_realtime");
    assert_provider_can_stream_text(&capabilities, "openrouter");
    assert_provider_can_stream_text(&capabilities, "z_ai");
}

#[test]
fn providers_declare_text_and_native_audio_output_capabilities() {
    let registry = capability_registry();

    for provider_id in [
        "cerebras",
        "claude",
        "codex",
        "gemini",
        "local",
        "openai_realtime",
        "openrouter",
        "z_ai",
    ] {
        let capabilities = registry.capabilities(provider_id).unwrap();
        assert_eq!(
            capabilities.final_text_output,
            LlmCapabilitySupport::Supported,
            "{provider_id} should declare final text output support"
        );
        assert_eq!(
            capabilities.streamed_text_output,
            LlmCapabilitySupport::Supported,
            "{provider_id} should declare streamed text output support"
        );
        assert_eq!(
            capabilities.native_audio_output,
            if matches!(provider_id, "openai_realtime" | "gemini") {
                LlmCapabilitySupport::ModelDependent
            } else {
                LlmCapabilitySupport::Unsupported
            },
            "{provider_id} should expose only its implemented native audio path"
        );
    }
}

#[test]
fn gemini_declares_runtime_live_multimodal_inputs() {
    let registry = capability_registry();

    let capabilities = registry.capabilities("gemini").unwrap();

    assert_eq!(
        capabilities.live_audio_input,
        LlmCapabilitySupport::Supported
    );
    assert_eq!(
        capabilities.screen_frame_broadcast_input,
        LlmCapabilitySupport::Supported
    );
    assert_eq!(
        capabilities.native_audio_output,
        LlmCapabilitySupport::ModelDependent
    );
}

#[test]
fn non_live_providers_do_not_claim_live_multimodal_inputs() {
    let registry = capability_registry();

    for provider_id in ["cerebras", "claude", "codex", "local", "openrouter", "z_ai"] {
        let capabilities = registry.capabilities(provider_id).unwrap();
        assert_eq!(
            capabilities.live_audio_input,
            LlmCapabilitySupport::Unsupported
        );
        assert_eq!(
            capabilities.screen_frame_broadcast_input,
            LlmCapabilitySupport::Unsupported
        );
    }
}

#[test]
fn openai_realtime_declares_live_multimodal_inputs() {
    let registry = capability_registry();

    let capabilities = registry.capabilities("openai_realtime").unwrap();

    assert_eq!(
        capabilities.live_audio_input,
        LlmCapabilitySupport::Supported
    );
    assert_eq!(
        capabilities.screen_frame_broadcast_input,
        LlmCapabilitySupport::Supported
    );
    assert_eq!(
        capabilities.image_snapshot_input,
        LlmCapabilitySupport::Supported
    );
}

#[test]
fn vision_input_is_explicitly_model_dependent() {
    let registry = capability_registry();

    for provider_id in ["cerebras", "claude", "openrouter"] {
        let capabilities = registry.capabilities(provider_id).unwrap();
        assert_eq!(
            capabilities.image_snapshot_input,
            LlmCapabilitySupport::ModelDependent
        );
    }
    let z_ai = registry.capabilities("z_ai").unwrap();
    assert_eq!(z_ai.image_snapshot_input, LlmCapabilitySupport::Unsupported);
    let gemini = registry.capabilities("gemini").unwrap();
    assert_eq!(
        gemini.image_snapshot_input,
        LlmCapabilitySupport::Unsupported
    );
}

fn capability_registry() -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs(
        vec![
            spawned_provider("claude", LlmProviderKind::Claude),
            spawned_provider("codex", LlmProviderKind::Codex),
            spawned_provider("gemini", LlmProviderKind::Gemini),
            spawned_provider("local", LlmProviderKind::Local),
            api_key_provider("openai_realtime", LlmProviderKind::OpenAiRealtime),
            remote_provider("cerebras", LlmProviderKind::Cerebras),
            remote_provider("openrouter", LlmProviderKind::OpenRouter),
            remote_provider("z_ai", LlmProviderKind::Zai),
        ],
        Arc::new(EmptyCapabilityHttpClient),
    )
    .unwrap()
}

fn api_key_provider(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: provider_id.to_string(),
        kind,
        model: "model".to_string(),
        endpoint: None,
        credential: Some("token".to_string()),
        completion_concurrency: None,
        spawn: None,
    }
}

fn assert_provider_can_stream_text(capabilities: &[LlmProviderCapabilities], provider_id: &str) {
    let found = capabilities
        .iter()
        .find(|capability| capability.provider_id == provider_id)
        .unwrap();
    assert_eq!(found.streamed_text_output, LlmCapabilitySupport::Supported);
}

fn remote_provider(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: provider_id.to_string(),
        kind,
        model: "model".to_string(),
        endpoint: Some("https://provider.test".to_string()),
        credential: Some("token".to_string()),
        completion_concurrency: None,
        spawn: None,
    }
}

fn spawned_provider(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: provider_id.to_string(),
        kind,
        model: "model".to_string(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "capability-only-provider".to_string(),
            args: Vec::new(),
            timeout_ms: 1000,
        }),
    }
}
