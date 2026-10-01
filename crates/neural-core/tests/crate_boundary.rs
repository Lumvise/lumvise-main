use lumvise_neural_core::{
    EngineConfig, LlmProviderConfig, LlmProviderKind, NeuralCoreConfig, SpawnConfig,
};

#[test]
fn crate_boundary_accepts_valid_empty_config() {
    let config = NeuralCoreConfig {
        providers: vec![],
        text2voice: None,
        voice2text: None,
        text2vector: None,
    };

    assert!(config.validate().is_ok());
}

#[test]
fn crate_boundary_rejects_empty_provider_id() {
    let config = NeuralCoreConfig {
        providers: vec![LlmProviderConfig {
            provider_id: String::new(),
            kind: LlmProviderKind::OpenRouter,
            model: "claude".to_string(),
            endpoint: Some("https://provider.test".to_string()),
            credential: Some("token".to_string()),
            completion_concurrency: None,
            spawn: None,
        }],
        text2voice: None,
        voice2text: None,
        text2vector: None,
    };

    let error = config.validate().unwrap_err().to_string();

    assert!(error.contains("expected non-empty provider id"));
}

#[test]
fn crate_boundary_rejects_empty_engine_command() {
    let config = NeuralCoreConfig {
        providers: vec![],
        text2voice: Some(EngineConfig {
            engine_id: "tts".to_string(),
            spawn: SpawnConfig {
                command: String::new(),
                args: vec![],
                timeout_ms: 1000,
            },
            expected_dimensions: None,
        }),
        voice2text: None,
        text2vector: None,
    };

    let error = config.validate().unwrap_err().to_string();

    assert!(error.contains("expected non-empty executable command"));
}

#[test]
fn crate_boundary_rejects_remote_provider_without_credential() {
    let config = LlmProviderConfig {
        provider_id: "openrouter".to_string(),
        kind: LlmProviderKind::OpenRouter,
        model: "openrouter/model".to_string(),
        endpoint: Some("https://provider.test".to_string()),
        credential: None,
        completion_concurrency: None,
        spawn: None,
    };

    let error = config.validate().unwrap_err().to_string();

    assert!(error.contains("expected non-empty provider credential"));
}
