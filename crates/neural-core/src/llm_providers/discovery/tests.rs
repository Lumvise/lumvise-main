#[test]
fn codex_availability_probe_disables_completion_notifications() {
    let args = super::cli_probe_args(crate::config::LlmProviderKind::Codex, "gpt-5.6-sol");
    assert!(args.windows(2).any(|pair| pair == ["-c", "notify=[]"]));
    assert_eq!(args.last().map(String::as_str), Some("Reply exactly OK"));
}

use super::*;
use parking_lot::Mutex;

struct FakeHttp {
    calls: Mutex<usize>,
}
impl LlmHttpClient for FakeHttp {
    fn post_json(&self, _: &LlmHttpRequest) -> Result<Value> {
        Ok(serde_json::json!({"choices":[{"message":{"content":"OK"}}]}))
    }
    fn get_json(&self, _: &LlmHttpRequest) -> Result<Value> {
        *self.calls.lock() += 1;
        Ok(serde_json::json!({"data":[{"id":"api-live"}]}))
    }
    fn stream_text(&self, _: &LlmHttpRequest) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}
struct FakeCommands {
    calls: Mutex<Vec<Vec<String>>>,
    inventory: Value,
}
impl LlmCommandTransport for FakeCommands {
    fn run(
        &self,
        _: &SpawnConfig,
        args: Vec<String>,
        _: Option<&str>,
    ) -> Result<ProviderCommandOutput> {
        self.calls.lock().push(args.clone());
        Ok(ProviderCommandOutput {
            stdout: if args.first().is_some_and(|arg| arg == "debug") {
                self.inventory.to_string()
            } else {
                "OK".to_string()
            },
            stderr: String::new(),
        })
    }
}
/// Every call fails, simulating a provider whose CLI probe itself is
/// rejected (e.g. by the remote API) rather than the transport layer.
struct FailingCommands;
impl LlmCommandTransport for FailingCommands {
    fn run(
        &self,
        _: &SpawnConfig,
        _: Vec<String>,
        _: Option<&str>,
    ) -> Result<ProviderCommandOutput> {
        Err(NeuralError::ProviderFailed {
            provider_id: "claude".to_string(),
            message: "usage limit exceeded".to_string(),
        })
    }
}
fn catalog() -> ProviderModelCatalog {
    ProviderModelCatalog::new(tempfile::tempdir().unwrap().path().join("catalog.toml")).unwrap()
}
fn codex() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "codex".into(),
        kind: LlmProviderKind::Codex,
        model: "provider-default".into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "codex".into(),
            args: vec![],
            timeout_ms: 1000,
        }),
    }
}
fn claude() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "claude".into(),
        kind: LlmProviderKind::Claude,
        model: "provider-default".into(),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: "claude".into(),
            args: vec![],
            timeout_ms: 1000,
        }),
    }
}

#[test]
fn client_sync_queries_only_client_inventory_and_probes_once() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let commands = Arc::new(FakeCommands {
        calls: Mutex::new(Vec::new()),
        inventory: serde_json::json!({"models":[{"id":"client-live"}]}),
    });
    let sync = LlmProviderSynchronizer::new(http.clone(), commands.clone(), catalog())
        .sync(vec![LlmProviderCandidate::Configured(codex())])
        .unwrap();
    let provider = &sync.catalog.providers[0];
    assert_eq!(provider.active_source, Some(LlmModelSource::Client));
    assert!(
        provider
            .model_sources
            .client
            .as_ref()
            .unwrap()
            .models
            .iter()
            .any(|model| model.id == "client-live")
    );
    assert_eq!(*http.calls.lock(), 0);
    assert_eq!(commands.calls.lock().len(), 2);
}

#[test]
fn model_refresh_lists_codex_without_inference_or_availability_claim() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let commands = Arc::new(FakeCommands {
        calls: Mutex::new(Vec::new()),
        inventory: serde_json::json!({"models":[{"slug":"gpt-6.1-sol","display_name":"GPT-6.1 Sol","visibility":"list"}]}),
    });
    let sync = LlmProviderSynchronizer::new(http, commands.clone(), catalog())
        .refresh_inventory(vec![LlmProviderCandidate::Configured(codex())])
        .unwrap();
    assert_eq!(
        commands.calls.lock().as_slice(),
        &[vec!["debug".to_string(), "models".to_string()]]
    );
    assert_eq!(
        sync.catalog.providers[0].state,
        LlmProviderAvailability::Unverified
    );
    assert!(sync.catalog.contains_model("codex", "gpt-6.1-sol"));
    assert!(sync.catalog.available_provider("codex").is_none());
}

#[test]
fn model_refresh_never_spawns_completion_when_no_listing_command_exists() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let commands = Arc::new(FakeCommands {
        calls: Mutex::new(Vec::new()),
        inventory: Value::Null,
    });
    let sync = LlmProviderSynchronizer::new(http, commands.clone(), catalog())
        .refresh_inventory(vec![LlmProviderCandidate::Configured(claude())])
        .unwrap();
    assert!(commands.calls.lock().is_empty());
    assert!(matches!(
        sync.catalog.providers[0].state,
        LlmProviderAvailability::DiscoveryFailed { .. }
    ));
    assert!(
        !sync.catalog.providers[0]
            .model_sources
            .client
            .as_ref()
            .unwrap()
            .models
            .is_empty()
    );
}

/// Claude Code has no non-interactive model-listing command (see
/// `cli_inventory_args`); sync must not spawn a doomed discovery call
/// for it and must still resolve + probe using the declared static
/// catalog default, exactly as it would if discovery had merely found
/// nothing extra.
#[test]
fn client_sync_skips_discovery_for_providers_with_no_listing_command() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let commands = Arc::new(FakeCommands {
        calls: Mutex::new(Vec::new()),
        inventory: Value::Null,
    });
    let sync = LlmProviderSynchronizer::new(http.clone(), commands.clone(), catalog())
        .sync(vec![LlmProviderCandidate::Configured(claude())])
        .unwrap();
    let provider = &sync.catalog.providers[0];
    assert_eq!(provider.state, LlmProviderAvailability::Available);
    // Only the probe call, never a discovery subprocess.
    let calls = commands.calls.lock();
    assert_eq!(calls.len(), 1);
    let probe = &calls[0];
    assert!(probe.iter().any(|arg| arg == "--strict-mcp-config"));
    assert!(
        probe
            .windows(2)
            .any(|pair| pair == ["--mcp-config", "{\"mcpServers\":{}}"])
    );
}

/// When a provider's probe fails for its own reason (e.g. the remote
/// API rejects the request) while discovery was also unavailable, the
/// surfaced message must include the probe's real, actionable error
/// rather than silently replacing it with the less relevant discovery
/// failure. Regression test for a bug where `InvocationFailed.message`
/// always preferred `discovery_error`, hiding the true cause.
#[test]
fn invocation_failure_message_surfaces_the_probes_own_error_not_only_discovery() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let sync = LlmProviderSynchronizer::new(http, Arc::new(FailingCommands), catalog())
        .sync(vec![LlmProviderCandidate::Configured(claude())])
        .unwrap();
    let provider = &sync.catalog.providers[0];
    let LlmProviderAvailability::InvocationFailed { message } = &provider.state else {
        panic!("expected InvocationFailed, got {:?}", provider.state);
    };
    assert!(
        message.contains("usage limit exceeded"),
        "message must surface the probe's real error: {message}"
    );
}

#[test]
fn codex_inventory_preserves_live_labels_order_and_filters_hidden_entries() {
    let inventory = serde_json::json!({"models":[
        {"slug":"gpt-6-astra","display_name":"GPT-6-Astra","visibility":"list"},
        {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list"},
        {"slug":"gpt-daybreak-blue-latest","display_name":"Daybreak Blue","visibility":"hide"},
        {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hidden"},
        {"slug":"gpt-6-sol","display_name":"GPT-6-Sol","visibility":"list"}
    ]});
    assert_eq!(
        parse_inventory(LlmProviderKind::Codex, inventory).unwrap(),
        vec![
            LlmModelDescriptor {
                id: "gpt-6-astra".into(),
                display_name: "GPT-6-Astra".into()
            },
            LlmModelDescriptor {
                id: "gpt-6.1-sol".into(),
                display_name: "GPT-6.1-Sol".into()
            },
            LlmModelDescriptor {
                id: "gpt-6-sol".into(),
                display_name: "GPT-6-Sol".into()
            },
        ]
    );
}

#[test]
fn inventory_deduplicates_ids_without_collapsing_duplicate_labels() {
    let inventory = serde_json::json!({"data":[
        {"id":"provider/z","name":"Shared label"},
        {"id":"provider/a","name":"Shared label"},
        {"id":"provider/z","name":"Later label"},
        {"id":"provider/blank","display_name":"  "},
        {"id":" ","display_name":"No ID"}
    ]});
    assert_eq!(
        parse_inventory(LlmProviderKind::OpenRouter, inventory).unwrap(),
        vec![
            LlmModelDescriptor {
                id: "provider/z".into(),
                display_name: "Shared label".into()
            },
            LlmModelDescriptor {
                id: "provider/a".into(),
                display_name: "Shared label".into()
            },
            LlmModelDescriptor {
                id: "provider/blank".into(),
                display_name: "provider/blank".into()
            },
        ]
    );
}

#[test]
fn gemini_inventory_keeps_generation_filter_and_display_names() {
    let inventory = serde_json::json!({"models":[
        {"name":"models/gemini-test","displayName":"Gemini Test",
         "supportedGenerationMethods":["generateContent"]},
        {"name":"models/embed-test","displayName":"Embedding Test",
         "supportedGenerationMethods":["embedContent"]},
        {"name":"models/gemini-unlabelled","supportedGenerationMethods":["generateContent"]}
    ]});
    assert_eq!(
        parse_inventory(LlmProviderKind::Gemini, inventory).unwrap(),
        vec![
            LlmModelDescriptor {
                id: "gemini-test".into(),
                display_name: "Gemini Test".into()
            },
            LlmModelDescriptor {
                id: "gemini-unlabelled".into(),
                display_name: "gemini-unlabelled".into()
            }
        ]
    );
}

#[test]
fn api_inventory_preserves_model_namespace_prefixes() {
    let inventory = serde_json::json!({"data":[{"id":"models/custom","display_name":"Custom"}]});
    assert_eq!(
        parse_inventory(LlmProviderKind::OpenAiCompatible, inventory).unwrap()[0].id,
        "models/custom"
    );
}

fn codex_inventory_sync(inventory: Value, model: &str) -> LlmProviderSync {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let commands = Arc::new(FakeCommands {
        calls: Mutex::new(Vec::new()),
        inventory,
    });
    let mut config = codex();
    config.model = model.into();
    LlmProviderSynchronizer::new(http, commands, catalog())
        .sync(vec![LlmProviderCandidate::Configured(config)])
        .unwrap()
}

#[test]
fn client_sync_publishes_only_live_visible_models_and_the_saved_missing_choice() {
    let sync = codex_inventory_sync(
        serde_json::json!({"models":[
            {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list"},
            {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide"}
        ]}),
        "saved-model",
    );
    let inventory = sync.catalog.providers[0]
        .model_sources
        .client
        .as_ref()
        .unwrap();
    assert_eq!(inventory.models.len(), 2);
    assert_eq!(inventory.models[0].display_name, "GPT-6.1-Sol");
    assert_eq!(
        inventory.models[1].display_name,
        "saved-model (unavailable)"
    );
    assert!(sync.catalog.contains_model("codex", "saved-model"));
    assert!(!sync.catalog.contains_model("codex", "gpt-5.4"));
    assert!(!sync.catalog.contains_model("codex", "codex-auto-review"));
    let mut request = adapter_probe_request(&codex());
    request.model = Some("saved-model".into());
    assert!(sync.registry.negotiate("codex", &request).is_ok());
}

#[test]
fn hidden_only_inventory_does_not_restore_the_bundled_catalog() {
    let sync = codex_inventory_sync(
        serde_json::json!({"models":[
            {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide"}
        ]}),
        "provider-default",
    );
    let provider = &sync.catalog.providers[0];
    assert!(matches!(
        provider.state,
        LlmProviderAvailability::DiscoveryFailed { .. }
    ));
    let inventory = provider.model_sources.client.as_ref().unwrap();
    assert!(inventory.models.is_empty());
    assert_eq!(inventory.default_model, None);
    assert!(sync.registry.provider_ids().is_empty());
}

#[test]
fn configured_hidden_model_is_retained_only_as_an_unavailable_choice() {
    let sync = codex_inventory_sync(
        serde_json::json!({"models":[
            {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide"}
        ]}),
        "codex-auto-review",
    );
    let inventory = sync.catalog.providers[0]
        .model_sources
        .client
        .as_ref()
        .unwrap();
    assert_eq!(
        inventory.models,
        vec![LlmModelDescriptor {
            id: "codex-auto-review".into(),
            display_name: "codex-auto-review (unavailable)".into()
        }]
    );
    assert_eq!(inventory.default_model, None);
}

#[test]
fn malformed_inventory_falls_back_but_valid_empty_inventory_is_authoritative() {
    assert!(
        parse_inventory(LlmProviderKind::Codex, serde_json::json!({"models": []}))
            .unwrap()
            .is_empty()
    );
    let error = parse_inventory(
        LlmProviderKind::Codex,
        serde_json::json!({"models":"invalid"}),
    )
    .unwrap_err();
    assert!(error.to_string().contains("invalid"));
    assert!(error.to_string().contains("array"));
    let sync = codex_inventory_sync(serde_json::json!({"wrong":[]}), "provider-default");
    let inventory = sync.catalog.providers[0]
        .model_sources
        .client
        .as_ref()
        .unwrap();
    assert_eq!(
        inventory.models,
        catalog().candidates(LlmProviderKind::Codex).models
    );
}

#[test]
fn offline_client_sync_preserves_the_saved_missing_model() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let mut config = codex();
    config.model = "offline-custom".into();
    let sync = LlmProviderSynchronizer::new(http, Arc::new(FailingCommands), catalog())
        .sync(vec![LlmProviderCandidate::Configured(config)])
        .unwrap();
    let provider = &sync.catalog.providers[0];
    assert!(matches!(
        provider.state,
        LlmProviderAvailability::InvocationFailed { .. }
    ));
    assert!(sync.catalog.contains_model("codex", "gpt-5.4"));
    assert!(sync.catalog.contains_model("codex", "offline-custom"));
}

#[test]
fn api_sync_queries_only_api_inventory() {
    let http = Arc::new(FakeHttp {
        calls: Mutex::new(0),
    });
    let commands = Arc::new(FakeCommands {
        calls: Mutex::new(Vec::new()),
        inventory: Value::Null,
    });
    let config = LlmProviderConfig {
        provider_id: "custom_openai".into(),
        kind: LlmProviderKind::OpenAiCompatible,
        model: "provider-default".into(),
        endpoint: Some("http://localhost/v1".into()),
        credential: None,
        completion_concurrency: None,
        spawn: None,
    };
    let sync = LlmProviderSynchronizer::new(http.clone(), commands.clone(), catalog())
        .sync(vec![LlmProviderCandidate::Configured(config)])
        .unwrap();
    let provider = &sync.catalog.providers[0];
    assert_eq!(provider.active_source, Some(LlmModelSource::Api));
    assert_eq!(
        provider.model_sources.api.as_ref().unwrap().models[0].id,
        "api-live"
    );
    assert!(provider.model_sources.client.is_none());
    assert_eq!(*http.calls.lock(), 1);
    assert!(commands.calls.lock().is_empty());
}
