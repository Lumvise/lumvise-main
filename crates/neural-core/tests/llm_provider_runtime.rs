use lumvise_neural_core::llm_providers::{
    LlmExecutionControl, LlmExecutorRegistry, LlmModalityInputKind, LlmStreamEvent,
};
use lumvise_neural_core::process::StreamControl;
use lumvise_neural_core::{LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, SpawnConfig};
use lumvise_resource_routing::InvocationControl;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const SPAWNED_FIXTURE: &str = env!("CARGO_BIN_EXE_spawned-engine-fixture");

#[path = "llm_provider_runtime/support.rs"]
mod support;
use support::{
    NamedFakeHttpClient, NamedRuntimeProvider, api_key_remote, fake_script, fresh_http_client,
    local_modality_error, registry_error_message, remote, request, request_with_mcp,
    spawned_provider,
};

#[test]
fn llm_provider_registry_resolves_all_configured_provider_kinds() {
    let temp = TempDir::new().unwrap();
    let claude = fake_script(
        &temp,
        "fake-claude",
        "printf '{\"result\":\"claude response\"}'",
    );
    let codex = fake_script(
        &temp,
        "fake-codex",
        r#"output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then shift; output="$1"; fi
  shift
done
printf '{"response":"codex response"}' > "$output""#,
    );
    let gemini = fake_script(
        &temp,
        "fake-gemini",
        "printf '{\"response\":\"gemini response\"}'",
    );
    let registry = LlmProviderRegistry::from_configs(
        vec![
            spawned_provider("claude", LlmProviderKind::Claude, claude),
            spawned_provider("codex", LlmProviderKind::Codex, codex),
            spawned_provider("gemini", LlmProviderKind::Gemini, gemini),
            api_key_remote("openai_realtime", LlmProviderKind::OpenAiRealtime),
            remote("cerebras", LlmProviderKind::Cerebras),
            remote("openrouter", LlmProviderKind::OpenRouter),
            remote("z_ai", LlmProviderKind::Zai),
            LlmProviderConfig {
                provider_id: "local".to_string(),
                kind: LlmProviderKind::Local,
                model: "local-model".to_string(),
                endpoint: None,
                credential: None,
                completion_concurrency: None,
                spawn: Some(SpawnConfig {
                    command: SPAWNED_FIXTURE.into(),
                    args: vec!["--mode".into(), "standard".into()],
                    timeout_ms: 1000,
                }),
            },
        ],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    )
    .unwrap();

    assert_eq!(
        registry.provider_ids(),
        vec![
            "cerebras",
            "claude",
            "codex",
            "gemini",
            "local",
            "openai_realtime",
            "openrouter",
            "z_ai"
        ]
    );
}

#[test]
fn cerebras_registry_build_rejects_missing_remote_requirements() {
    let complete = LlmProviderConfig {
        provider_id: "cerebras".to_string(),
        kind: LlmProviderKind::Cerebras,
        model: "gemma-4-31b".to_string(),
        endpoint: Some("https://api.cerebras.ai/v1".to_string()),
        credential: Some("test-token".to_string()),
        completion_concurrency: None,
        spawn: None,
    };
    assert!(
        LlmProviderRegistry::from_configs(vec![complete.clone()], fresh_http_client(),).is_ok()
    );

    let mut missing_endpoint = complete.clone();
    missing_endpoint.endpoint = None;
    let endpoint_error = registry_error_message(missing_endpoint);
    assert!(
        endpoint_error.contains("endpoint"),
        "cerebras missing endpoint rejection: {endpoint_error}"
    );

    let mut missing_credential = complete.clone();
    missing_credential.credential = None;
    let credential_error = registry_error_message(missing_credential);
    assert!(
        credential_error.contains("credential"),
        "cerebras missing credential rejection: {credential_error}"
    );

    let mut empty_model = complete.clone();
    empty_model.model = String::new();
    let model_error = registry_error_message(empty_model);
    assert!(
        model_error.contains("model"),
        "cerebras empty model rejection: {model_error}"
    );
}

#[test]
fn llm_provider_registry_accepts_prebuilt_provider_instances() {
    let registry = LlmProviderRegistry::from_provider_instances(vec![Box::new(
        NamedRuntimeProvider::new("dummy"),
    )])
    .unwrap();

    assert_eq!(registry.provider_ids(), vec!["dummy"]);
    assert_eq!(
        registry
            .stream("dummy", &request(true), StreamControl::unbounded())
            .unwrap(),
        vec![
            LlmStreamEvent::ContentDelta {
                text: "dummy delta".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
}

#[test]
fn llm_executor_registry_runs_with_one_shared_invocation_control() {
    let providers = Arc::new(Mutex::new(
        LlmProviderRegistry::from_provider_instances(vec![Box::new(NamedRuntimeProvider::new(
            "dummy",
        ))])
        .unwrap(),
    ));
    let executors = LlmExecutorRegistry::new(Arc::clone(&providers));
    let control = InvocationControl::sixty_seconds();

    let response = executors
        .complete(
            "dummy",
            request(false),
            LlmExecutionControl::new(control.clone()),
        )
        .unwrap();
    assert_eq!(response.content, "dummy delta");

    control.cancel();
    assert!(matches!(
        executors.complete("dummy", request(false), LlmExecutionControl::new(control),),
        Err(lumvise_neural_core::llm_providers::LlmFailure {
            tier: lumvise_neural_core::llm_providers::LlmFailureTier::Session,
            code: lumvise_neural_core::llm_providers::LlmFailureCode::Cancelled,
            ..
        })
    ));
}

#[test]
fn llm_provider_registry_rejects_duplicate_provider_instances() {
    let result = LlmProviderRegistry::from_provider_instances(vec![
        Box::new(NamedRuntimeProvider::new("dummy")),
        Box::new(NamedRuntimeProvider::new("dummy")),
    ]);
    let error = result.err().unwrap().to_string();

    assert!(error.contains("dummy"));
    assert!(error.contains("unique provider id"));
}

#[test]
fn llm_provider_routes_remote_final_and_streaming_responses() {
    let registry = LlmProviderRegistry::from_configs(
        vec![
            remote("openrouter", LlmProviderKind::OpenRouter),
            remote("z_ai", LlmProviderKind::Zai),
        ],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    )
    .unwrap();

    let response = registry.complete("openrouter", &request(false)).unwrap();
    let z_ai_response = registry.complete("z_ai", &request(false)).unwrap();
    let events = registry
        .stream("openrouter", &request(true), StreamControl::unbounded())
        .unwrap();

    // OpenRouter completions stream (large non-streamed OpenRouter responses were
    // prone to a mid-transfer decode failure), so complete() reuses the same SSE
    // fixture as the explicit stream() call below.
    assert_eq!(response.content, "hello");
    assert_eq!(z_ai_response.content, "openrouter response");
    assert_eq!(
        events,
        vec![
            LlmStreamEvent::ContentDelta {
                text: "hel".to_string()
            },
            LlmStreamEvent::ContentDelta {
                text: "lo".to_string()
            },
            LlmStreamEvent::Complete,
        ]
    );
}

#[test]
fn llm_provider_stream_cancellation_emits_cancelled_event() {
    let registry = LlmProviderRegistry::from_configs(
        vec![remote("openrouter", LlmProviderKind::OpenRouter)],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    )
    .unwrap();

    let events = registry
        .stream("openrouter", &request(true), StreamControl::cancel_after(1))
        .unwrap();

    assert_eq!(events.last(), Some(&LlmStreamEvent::Cancelled));
}

#[test]
fn llm_provider_reports_unknown_id_missing_credential_and_bad_stream() {
    let missing_credential = match LlmProviderRegistry::from_configs(
        vec![LlmProviderConfig {
            credential: None,
            ..remote("openrouter", LlmProviderKind::OpenRouter)
        }],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    ) {
        Ok(_) => panic!("expected missing credential error"),
        Err(error) => error.to_string(),
    };
    let registry = LlmProviderRegistry::from_configs(
        vec![remote("openrouter", LlmProviderKind::OpenRouter)],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: true,
        }),
    )
    .unwrap();
    let unknown = registry
        .complete("missing", &request(false))
        .unwrap_err()
        .to_string();
    let bad_stream = registry
        .stream("openrouter", &request(true), StreamControl::unbounded())
        .unwrap_err()
        .to_string();

    assert!(missing_credential.contains("expected non-empty provider credential"));
    assert!(unknown.contains("expected configured provider id"));
    assert!(bad_stream.contains("OpenRouter content text"));
}

#[test]
fn local_llm_provider_reports_process_failure() {
    let registry = LlmProviderRegistry::from_configs(
        vec![LlmProviderConfig {
            provider_id: "local".to_string(),
            kind: LlmProviderKind::Local,
            model: "local-model".to_string(),
            endpoint: None,
            credential: None,
            completion_concurrency: None,
            spawn: Some(SpawnConfig {
                command: SPAWNED_FIXTURE.into(),
                args: vec!["--mode".into(), "failure".into()],
                timeout_ms: 1000,
            }),
        }],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    )
    .unwrap();

    let error = registry
        .complete("local", &request(false))
        .unwrap_err()
        .to_string();

    assert!(error.contains("fixture provider failure"));
}

#[test]
fn local_llm_provider_rejects_live_modality_inputs() {
    let registry = LlmProviderRegistry::from_configs(
        vec![spawned_provider(
            "local",
            LlmProviderKind::Local,
            SPAWNED_FIXTURE.into(),
        )],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    )
    .unwrap();
    let audio_error = local_modality_error(&registry, LlmModalityInputKind::LiveAudioChunk);
    let frame_error = local_modality_error(&registry, LlmModalityInputKind::ScreenFrame);

    assert!(audio_error.contains("live_audio_input is unsupported"));
    assert!(frame_error.contains("screen_frame_broadcast_input is unsupported"));
}

#[test]
fn local_llm_provider_forwards_assistant_mcp_servers_in_protobuf() {
    let registry = LlmProviderRegistry::from_configs(
        vec![LlmProviderConfig {
            provider_id: "local".to_string(),
            kind: LlmProviderKind::Local,
            model: "local-model".to_string(),
            endpoint: None,
            credential: None,
            completion_concurrency: None,
            spawn: Some(SpawnConfig {
                command: SPAWNED_FIXTURE.into(),
                args: vec!["--mode".into(), "require-mcp".into()],
                timeout_ms: 5_000,
            }),
        }],
        Arc::new(NamedFakeHttpClient {
            malformed_stream: false,
        }),
    )
    .unwrap();

    let response = registry
        .complete("local", &request_with_mcp(false))
        .unwrap();

    assert_eq!(response.content, "fixture response");
}
