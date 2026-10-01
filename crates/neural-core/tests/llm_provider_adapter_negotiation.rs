use lumvise_neural_core::llm_providers::adapter::{
    LlmAdapterPreferences, LlmLatencyClass, LlmMcpToolMode, LlmSessionSupport, LlmTransportKind,
    LlmTransportPreference,
};
use lumvise_neural_core::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use lumvise_neural_core::llm_providers::{
    LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest,
};
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingHttpClient {
    calls: Mutex<Vec<LlmHttpRequest>>,
}

impl RecordingHttpClient {
    fn calls(&self) -> Vec<LlmHttpRequest> {
        self.calls.lock().unwrap().clone()
    }
}

impl LlmHttpClient for RecordingHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(json!({ "content": [{ "type": "text", "text": "ok" }] }))
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(Vec::new())
    }
}

#[test]
fn direct_api_preference_selects_direct_transport_when_credentials_exist() {
    let registry = registry(
        vec![
            direct_config("cerebras", LlmProviderKind::Cerebras),
            direct_config("claude", LlmProviderKind::Claude),
            direct_config("gemini", LlmProviderKind::Gemini),
            direct_config("codex", LlmProviderKind::Codex),
            direct_config("openrouter", LlmProviderKind::OpenRouter),
            direct_config("z_ai", LlmProviderKind::Zai),
        ],
        direct_preferences(),
        Arc::new(RecordingHttpClient::default()),
    );

    for provider_id in [
        "cerebras",
        "claude",
        "gemini",
        "codex",
        "openrouter",
        "z_ai",
    ] {
        let negotiated = registry.negotiate(provider_id, &text_request()).unwrap();
        assert_eq!(negotiated.selected_transport, LlmTransportKind::DirectApi);
        assert!(negotiated.direct_api_active);
        assert_eq!(negotiated.mcp_tool_mode, LlmMcpToolMode::FunctionToolBridge);
    }
}

#[test]
fn auto_route_prefers_direct_api_only_with_credential_and_cli_otherwise() {
    let registry = registry(
        vec![
            config_with_spawn_and_credential("claude", LlmProviderKind::Claude),
            client_config("gemini", LlmProviderKind::Gemini),
        ],
        LlmAdapterPreferences::default(),
        Arc::new(RecordingHttpClient::default()),
    );
    assert_eq!(
        registry
            .negotiate("claude", &text_request())
            .unwrap()
            .selected_transport,
        LlmTransportKind::DirectApi
    );
    assert_eq!(
        registry
            .negotiate("gemini", &text_request())
            .unwrap()
            .selected_transport,
        LlmTransportKind::Client
    );
}

#[test]
fn z_ai_direct_api_does_not_advertise_snapshot_inputs() {
    let registry = registry(
        vec![direct_config("z_ai", LlmProviderKind::Zai)],
        direct_preferences(),
        Arc::new(RecordingHttpClient::default()),
    );

    let negotiated = registry.negotiate("z_ai", &text_request()).unwrap();

    assert_eq!(negotiated.selected_transport, LlmTransportKind::DirectApi);
    assert!(!negotiated.image_snapshot_input);
}

#[test]
fn client_preference_selects_client_without_api_credentials() {
    let registry = registry(
        vec![
            client_config("claude", LlmProviderKind::Claude),
            client_config("gemini", LlmProviderKind::Gemini),
            client_config("codex", LlmProviderKind::Codex),
            client_config("local", LlmProviderKind::Local),
        ],
        client_preferences(),
        Arc::new(RecordingHttpClient::default()),
    );

    for provider_id in ["claude", "gemini", "codex"] {
        let negotiated = registry.negotiate(provider_id, &text_request()).unwrap();
        assert_eq!(negotiated.selected_transport, LlmTransportKind::Client);
        assert!(negotiated.client_transport_active);
        assert_eq!(negotiated.mcp_tool_mode, LlmMcpToolMode::NativeMcp);
    }
    let local = registry.negotiate("local", &text_request()).unwrap();
    assert_eq!(local.selected_transport, LlmTransportKind::LocalProcess);
}

#[test]
fn direct_api_preference_falls_back_to_client_when_allowed() {
    let registry = registry(
        vec![client_config("claude", LlmProviderKind::Claude)],
        direct_preferences(),
        Arc::new(RecordingHttpClient::default()),
    );

    let negotiated = registry.negotiate("claude", &text_request()).unwrap();

    assert_eq!(negotiated.selected_transport, LlmTransportKind::Client);
    assert!(
        negotiated
            .degraded_reasons
            .iter()
            .any(|reason| reason.contains("direct API credential"))
    );
}

#[test]
fn direct_api_preference_without_fallback_rejects_missing_credentials() {
    let result = LlmProviderRegistry::from_configs_with_preferences(
        vec![client_config("claude", LlmProviderKind::Claude)],
        Arc::new(RecordingHttpClient::default()),
        LlmAdapterPreferences {
            transport_preference: LlmTransportPreference::PreferDirectApi,
            allow_fallbacks: false,
        },
    );

    let error = match result {
        Ok(_) => panic!("direct API without fallback should fail during initialization"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("direct API credential"));
    assert!(error.contains("claude"));
}

#[test]
fn cerebras_direct_api_selects_function_tool_bridge_and_rejects_live_inputs() {
    let http_client = Arc::new(RecordingHttpClient::default());
    let registry = registry(
        vec![direct_config("cerebras", LlmProviderKind::Cerebras)],
        direct_preferences(),
        http_client.clone(),
    );

    let negotiated = registry.negotiate("cerebras", &text_request()).unwrap();
    assert_eq!(negotiated.selected_transport, LlmTransportKind::DirectApi);
    assert!(negotiated.direct_api_active);
    assert!(!negotiated.live_streaming_active);
    assert_eq!(negotiated.mcp_tool_mode, LlmMcpToolMode::FunctionToolBridge);

    for (kind, needle) in [
        (
            LlmModalityInputKind::LiveAudioChunk,
            "live_audio_input is unsupported",
        ),
        (
            LlmModalityInputKind::ScreenFrame,
            "screen_frame_broadcast_input is unsupported",
        ),
    ] {
        let error = registry
            .stream(
                "cerebras",
                &modality_request(kind),
                lumvise_neural_core::process::StreamControl::unbounded(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(needle),
            "cerebras expected {needle}, got: {error}"
        );
    }
    assert!(http_client.calls().is_empty());
}

#[test]
fn live_request_for_text_only_provider_fails_before_http_execution() {
    let http_client = Arc::new(RecordingHttpClient::default());
    let registry = registry(
        vec![direct_config("claude", LlmProviderKind::Claude)],
        direct_preferences(),
        http_client.clone(),
    );

    let error = registry
        .stream(
            "claude",
            &live_request(),
            lumvise_neural_core::process::StreamControl::unbounded(),
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("live_audio_input is unsupported"));
    assert!(http_client.calls().is_empty());
}

#[test]
fn live_preference_selects_live_transport_for_live_capable_providers() {
    let registry = registry(
        vec![
            direct_config("gemini", LlmProviderKind::Gemini),
            direct_config("openai_realtime", LlmProviderKind::OpenAiRealtime),
        ],
        LlmAdapterPreferences {
            transport_preference: LlmTransportPreference::PreferLive,
            allow_fallbacks: true,
        },
        Arc::new(RecordingHttpClient::default()),
    );

    for provider_id in ["gemini", "openai_realtime"] {
        let negotiated = registry.negotiate(provider_id, &live_request()).unwrap();
        assert_eq!(negotiated.selected_transport, LlmTransportKind::Live);
        assert!(negotiated.live_streaming_active);
        assert_eq!(negotiated.latency_class, LlmLatencyClass::Low);
        assert_ne!(negotiated.session_support, LlmSessionSupport::Unsupported);
    }
}

#[test]
fn live_request_can_override_direct_preference_when_fallback_is_allowed() {
    let registry = registry(
        vec![direct_config("gemini", LlmProviderKind::Gemini)],
        direct_preferences(),
        Arc::new(RecordingHttpClient::default()),
    );

    let negotiated = registry.negotiate("gemini", &live_request()).unwrap();

    assert_eq!(negotiated.selected_transport, LlmTransportKind::Live);
    assert!(negotiated.live_streaming_active);
}

fn registry(
    configs: Vec<LlmProviderConfig>,
    preferences: LlmAdapterPreferences,
    http_client: Arc<dyn LlmHttpClient>,
) -> LlmProviderRegistry {
    LlmProviderRegistry::from_configs_with_preferences(configs, http_client, preferences).unwrap()
}

fn direct_preferences() -> LlmAdapterPreferences {
    LlmAdapterPreferences {
        transport_preference: LlmTransportPreference::PreferDirectApi,
        allow_fallbacks: true,
    }
}

fn client_preferences() -> LlmAdapterPreferences {
    LlmAdapterPreferences {
        transport_preference: LlmTransportPreference::PreferClient,
        allow_fallbacks: true,
    }
}

fn text_request() -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream: true,
        provider_id: None,
        model: None,
        conversation_id: Some("conversation-a".to_string()),
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

fn live_request() -> LlmRequest {
    let mut request = text_request();
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "audio-1".to_string(),
        kind: LlmModalityInputKind::LiveAudioChunk,
        media_type: "audio/pcm;rate=16000".to_string(),
        bytes: vec![1, 2],
        metadata: json!({}),
    }];
    request
}

fn modality_request(kind: LlmModalityInputKind) -> LlmRequest {
    let mut request = text_request();
    request.modality_inputs = vec![LlmModalityInput {
        input_id: "modality-1".to_string(),
        kind,
        media_type: "application/octet-stream".to_string(),
        bytes: vec![1],
        metadata: json!({}),
    }];
    request
}

fn direct_config(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
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

fn client_config(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: provider_id.to_string(),
        kind,
        model: "model".to_string(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "fake-client".to_string(),
            args: Vec::new(),
            timeout_ms: 1000,
        }),
    }
}
fn config_with_spawn_and_credential(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
    LlmProviderConfig {
        credential: Some("token".to_string()),
        ..client_config(provider_id, kind)
    }
}
