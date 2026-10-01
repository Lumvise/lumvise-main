use lumvise_neural_core::llm_providers::capabilities::{
    LlmCapabilitySupport, LlmProviderCapabilities,
};
use lumvise_neural_core::llm_providers::contract::{
    LlmHttpClient, LlmHttpRequest, LlmProvider, LlmStreamEventSink,
};
use lumvise_neural_core::llm_providers::{
    LlmMcpServerConfig, LlmMessage, LlmModalityInput, LlmModalityInputKind, LlmRequest,
    LlmResponse, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tempfile::TempDir;

#[derive(Clone)]
pub struct NamedFakeHttpClient {
    pub malformed_stream: bool,
}

impl LlmHttpClient for NamedFakeHttpClient {
    fn post_json(&self, request: &LlmHttpRequest) -> lumvise_neural_core::Result<Value> {
        assert_eq!(request.endpoint, "https://provider.test/chat/completions");
        assert_eq!(request.credential, "token");
        Ok(json!({
            "choices": [{"message": {"content": "openrouter response"}}],
            "metadata": { "observed_model": request.payload["model"].clone() }
        }))
    }

    fn stream_text(&self, _request: &LlmHttpRequest) -> lumvise_neural_core::Result<Vec<String>> {
        if self.malformed_stream {
            return Ok(vec![
                "data: {\"choices\":[{\"delta\":{\"content\":42}}]}\n\n".to_string(),
            ]);
        }
        Ok(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n".to_string(),
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ])
    }
}

pub fn fresh_http_client() -> Arc<NamedFakeHttpClient> {
    Arc::new(NamedFakeHttpClient {
        malformed_stream: false,
    })
}

pub fn registry_error_message(config: LlmProviderConfig) -> String {
    LlmProviderRegistry::from_configs(vec![config], fresh_http_client())
        .err()
        .map(|error| error.to_string())
        .unwrap_or_else(|| panic!("expected registry build failure for cerebras"))
}

pub struct NamedRuntimeProvider {
    provider_id: String,
}

impl NamedRuntimeProvider {
    pub fn new(provider_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
        }
    }
}

impl LlmProvider for NamedRuntimeProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: self.provider_id.clone(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, request: &LlmRequest) -> lumvise_neural_core::Result<LlmResponse> {
        Ok(LlmResponse {
            provider_id: self.provider_id.clone(),
            model: request.model.clone().unwrap_or_else(|| "dummy".to_string()),
            content: "dummy delta".to_string(),
            metadata: json!({}),
        })
    }

    fn stream_with_events(
        &self,
        _request: &LlmRequest,
        _control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> lumvise_neural_core::Result<()> {
        on_event(LlmStreamEvent::ContentDelta {
            text: "dummy delta".to_string(),
        })?;
        on_event(LlmStreamEvent::Complete)
    }
}

pub fn remote(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
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

pub fn api_key_remote(provider_id: &str, kind: LlmProviderKind) -> LlmProviderConfig {
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

pub fn spawned_provider(
    provider_id: &str,
    kind: LlmProviderKind,
    command: String,
) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: provider_id.to_string(),
        kind,
        model: "model".to_string(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command,
            args: vec![],
            timeout_ms: 1000,
        }),
    }
}

pub fn fake_script(temp: &TempDir, name: &str, body: &str) -> String {
    let path = temp.path().join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path.to_string_lossy().to_string()
}

pub fn local_modality_error(registry: &LlmProviderRegistry, kind: LlmModalityInputKind) -> String {
    let mut live_request = request(false);
    live_request.modality_inputs = vec![modality_input(kind)];

    registry
        .complete("local", &live_request)
        .unwrap_err()
        .to_string()
}

pub fn request(stream: bool) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
        stream,
        provider_id: None,
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

pub fn request_with_mcp(stream: bool) -> LlmRequest {
    let mut request = request(stream);
    request.mcp_servers = vec![LlmMcpServerConfig {
        name: "lumvise-assistant".to_string(),
        url: "http://127.0.0.1:4180/mcp/sse/builtin.assistant/session-a".to_string(),
    }];
    request
}

pub fn modality_input(kind: LlmModalityInputKind) -> LlmModalityInput {
    LlmModalityInput {
        input_id: "modality-1".to_string(),
        kind,
        media_type: "application/octet-stream".to_string(),
        bytes: vec![1],
        metadata: json!({}),
    }
}
