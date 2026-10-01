use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{LlmMessage, LlmRequest};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::Value;
use std::sync::Arc;

struct NoHttp;

impl LlmHttpClient for NoHttp {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        panic!(
            "live CLI canary unexpectedly called HTTP: {}",
            request.endpoint
        )
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        panic!(
            "live CLI canary unexpectedly streamed HTTP: {}",
            request.endpoint
        )
    }
}

#[test]
#[ignore = "explicit live provider CLI canary; run scripts/canaries/provider-cli"]
fn installed_provider_cli_completes_through_public_interface() {
    let provider = std::env::var("LUMVISE_LIVE_PROVIDER")
        .expect("LUMVISE_LIVE_PROVIDER must name codex or gemini");
    let registry =
        LlmProviderRegistry::from_configs(vec![live_config(&provider)], Arc::new(NoHttp)).unwrap();
    let response = registry.complete(&provider, &canary_request()).unwrap();

    assert!(!response.content.trim().is_empty());
}

fn live_config(provider: &str) -> LlmProviderConfig {
    let (kind, model) = match provider {
        "codex" => (LlmProviderKind::Codex, "provider-default"),
        "gemini" => (LlmProviderKind::Gemini, "gemini-2.5-flash"),
        value => panic!("unsupported live provider `{value}`; expected codex or gemini"),
    };
    LlmProviderConfig {
        provider_id: provider.into(),
        kind,
        model: model.into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: provider.into(),
            args: vec![],
            timeout_ms: 60_000,
        }),
    }
}

fn canary_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".into(),
            content: "Reply with exactly: lumvise-provider-canary-ok".into(),
        }],
        stream: false,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: vec![],
        modality_inputs: vec![],
    }
}
