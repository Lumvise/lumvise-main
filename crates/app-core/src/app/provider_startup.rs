//! Provider configuration and discovery shared by desktop and community startup.
//! Startup installs callable configurations first; discovery alone reconciles catalogs.

use super::provider_settings::{stored_provider_api_key, stored_provider_endpoint};
use crate::AppCore;
use lumvise_db_core::{RelationalOperation, RelationalPersistence, RelationalResult};
use lumvise_frontend_core::{
    AppSettings, AssistantModelOption, AssistantModelSource, AssistantProviderCatalog,
    AssistantProviderOption,
};
use lumvise_neural_core::llm_providers::contract::LlmHttpClient;
use lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient;
use lumvise_neural_core::{
    LlmConfigurationRequirement, LlmModelSource, LlmProviderAvailability, LlmProviderCandidate,
    LlmProviderCatalog, LlmProviderConfig, LlmProviderKind, LlmProviderRegistry, LlmProviderSync,
    LlmProviderSynchronizer, SpawnConfig,
};
use lumvise_resource_routing::InvocationControl;
use std::sync::Arc;
use tracing::error;

/// Synchronizes local LLM providers off the startup path. Provider discovery
/// probes each provider's network endpoint or CLI subprocess and can take up
/// to the configured request timeout; running it before the app was ready
/// blocked the Workspace window from opening until every candidate settled.
/// Startup already installs configured providers without probing them. This
/// refreshes their model inventory and availability once discovery completes.
pub(super) fn spawn_llm_provider_sync(
    app: Arc<AppCore>,
    relational: Arc<dyn RelationalPersistence>,
) {
    let spawned = std::thread::Builder::new()
        .name("lumvise-llm-provider-sync".to_string())
        .spawn(move || {
            let sync = match synchronize_llm_providers(relational.as_ref()) {
                Ok(sync) => sync,
                Err(error) => {
                    error!(target: "app-core::desktop", event = "llm_provider_sync_failed", error = %error, "failed to synchronize local LLM providers");
                    return;
                }
            };
            let catalog = assistant_provider_catalog(&sync.catalog);
            if let Err(error) = app.replace_synchronized_providers(sync) {
                error!(target: "app-core::desktop", event = "llm_provider_sync_apply_failed", error = %error, "failed to apply synchronized LLM providers");
                return;
            }
            if let Err(error) = app.frontend().replace_assistant_provider_catalog(catalog) {
                error!(target: "app-core::desktop", event = "llm_provider_catalog_publish_failed", error = %error, "failed to publish synchronized LLM provider catalog to frontend");
            }
        });
    if let Err(error) = spawned {
        error!(target: "app-core::desktop", event = "llm_provider_sync_spawn_failed", error = %error, "failed to spawn LLM provider sync thread");
    }
}

pub(super) fn persisted_app_settings(
    relational: &dyn RelationalPersistence,
) -> Result<AppSettings, Box<dyn std::error::Error>> {
    let result = relational.execute(
        RelationalOperation::GetPersistentSetting {
            scope: "frontend".to_string(),
            key: "app_settings".to_string(),
        },
        &InvocationControl::sixty_seconds(),
    )?;
    let RelationalResult::PersistentSetting(record) = result else {
        return Err("app settings returned an unexpected relational result".into());
    };
    let Some(record) = record else {
        return Ok(AppSettings::default());
    };
    Ok(serde_json::from_value(record.value)?)
}

pub(crate) fn synchronize_llm_providers(
    relational: &dyn RelationalPersistence,
) -> Result<LlmProviderSync, Box<dyn std::error::Error>> {
    LlmProviderSynchronizer::production(Arc::new(ReqwestLlmHttpClient::new()))?
        .sync(configured_llm_candidates(relational)?)
        .map_err(Into::into)
}

/// Updates selector inventory only; the Refresh models action must not buy inference.
/// For example, Settings refresh uses this while startup may explicitly probe providers.
pub(crate) fn refresh_llm_inventory(
    relational: &dyn RelationalPersistence,
) -> Result<LlmProviderSync, Box<dyn std::error::Error>> {
    LlmProviderSynchronizer::production(Arc::new(ReqwestLlmHttpClient::new()))?
        .refresh_inventory(configured_llm_candidates(relational)?)
        .map_err(Into::into)
}

/// Makes configured providers callable before optional discovery probes finish.
/// Model requests still carry the user's saved selection; catalog reconciliation
/// stays with the background sync so an empty inventory cannot erase it.
pub(super) fn configured_llm_registry(
    relational: &dyn RelationalPersistence,
    http: Arc<dyn LlmHttpClient>,
) -> Result<LlmProviderRegistry, Box<dyn std::error::Error>> {
    let configs = configured_llm_candidates(relational)?
        .into_iter()
        .filter_map(|candidate| match candidate {
            LlmProviderCandidate::Configured(config) if config.validate().is_ok() => Some(config),
            _ => None,
        })
        .collect();
    LlmProviderRegistry::from_configs(configs, http).map_err(Into::into)
}

#[cfg(test)]
#[path = "executor_startup_tests.rs"]
mod startup_tests;

fn configured_llm_candidates(
    relational: &dyn RelationalPersistence,
) -> Result<Vec<LlmProviderCandidate>, Box<dyn std::error::Error>> {
    let mut candidates = vec![
        LlmProviderCandidate::Configured(codex_provider_config()),
        LlmProviderCandidate::Configured(claude_provider_config()),
        LlmProviderCandidate::Configured(gemini_provider_config(relational)?),
        remote_candidate(
            "openai_realtime",
            LlmProviderKind::OpenAiRealtime,
            openai_audio_provider_config(relational)?,
        ),
    ];
    candidates.push(remote_candidate(
        "cerebras",
        LlmProviderKind::Cerebras,
        cerebras_provider_config(relational)?,
    ));
    candidates.push(remote_candidate(
        "openrouter",
        LlmProviderKind::OpenRouter,
        openrouter_provider_config(relational)?,
    ));
    candidates.push(remote_candidate(
        "z_ai",
        LlmProviderKind::Zai,
        z_ai_provider_config(relational)?,
    ));
    candidates.push(custom_openai_candidate(relational)?);
    Ok(candidates)
}

fn remote_candidate(
    provider_id: &str,
    kind: LlmProviderKind,
    config: Option<LlmProviderConfig>,
) -> LlmProviderCandidate {
    config
        .map(LlmProviderCandidate::Configured)
        .unwrap_or_else(|| LlmProviderCandidate::MissingConfiguration {
            provider_id: provider_id.to_string(),
            kind,
            missing: LlmConfigurationRequirement::Credential,
        })
}

/// The user-defined OpenAI-compatible endpoint requires a stored endpoint
/// URL (API key optional), so its missing-configuration requirement is
/// `Endpoint`, unlike the credential-gated providers above.
fn custom_openai_candidate(
    relational: &dyn RelationalPersistence,
) -> Result<LlmProviderCandidate, Box<dyn std::error::Error>> {
    let Some(endpoint) = env_first(&["LUMVISE_CUSTOM_OPENAI_ENDPOINT"])
        .or(stored_provider_endpoint(relational, "custom_openai")?)
    else {
        return Ok(LlmProviderCandidate::MissingConfiguration {
            provider_id: "custom_openai".to_string(),
            kind: LlmProviderKind::OpenAiCompatible,
            missing: LlmConfigurationRequirement::Endpoint,
        });
    };
    let credential = env_first(&["LUMVISE_CUSTOM_OPENAI_API_KEY"])
        .or(stored_provider_api_key(relational, "custom_openai")?);
    Ok(LlmProviderCandidate::Configured(LlmProviderConfig {
        provider_id: "custom_openai".to_string(),
        kind: LlmProviderKind::OpenAiCompatible,
        model: env_or_default("LUMVISE_CUSTOM_OPENAI_MODEL", "provider-default"),
        endpoint: Some(endpoint),
        credential,
        completion_concurrency: None,
        spawn: None,
    }))
}

fn codex_provider_config() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "codex".to_string(),
        kind: LlmProviderKind::Codex,
        model: env_or_default("LUMVISE_CODEX_MODEL", "provider-default"),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: env_or_default("LUMVISE_CODEX_COMMAND", "codex"),
            args: Vec::new(),
            timeout_ms: codex_timeout_ms(),
        }),
    }
}

#[cfg(feature = "assistant-e2e")]
fn codex_timeout_ms() -> u64 {
    5_000
}

#[cfg(not(feature = "assistant-e2e"))]
fn codex_timeout_ms() -> u64 {
    180_000
}

fn claude_provider_config() -> LlmProviderConfig {
    LlmProviderConfig {
        provider_id: "claude".to_string(),
        kind: LlmProviderKind::Claude,
        model: env_or_default("LUMVISE_CLAUDE_MODEL", "provider-default"),
        endpoint: None,
        credential: None,
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: env_or_default("LUMVISE_CLAUDE_COMMAND", "claude"),
            args: Vec::new(),
            timeout_ms: 180_000,
        }),
    }
}

fn gemini_provider_config(
    relational: &dyn RelationalPersistence,
) -> Result<LlmProviderConfig, Box<dyn std::error::Error>> {
    Ok(LlmProviderConfig {
        provider_id: "gemini".to_string(),
        kind: LlmProviderKind::Gemini,
        model: env_or_default("LUMVISE_GEMINI_MODEL", "provider-default"),
        endpoint: env_first(&["LUMVISE_GEMINI_LIVE_ENDPOINT"]),
        credential: env_first(&["LUMVISE_GEMINI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"])
            .or(stored_provider_api_key(relational, "gemini")?),
        completion_concurrency: None,
        spawn: Some(SpawnConfig {
            command: env_or_default("LUMVISE_GEMINI_COMMAND", "gemini"),
            args: Vec::new(),
            timeout_ms: 180_000,
        }),
    })
}

fn openai_audio_provider_config(
    relational: &dyn RelationalPersistence,
) -> Result<Option<LlmProviderConfig>, Box<dyn std::error::Error>> {
    let credential = env_first(&["LUMVISE_OPENAI_API_KEY", "OPENAI_API_KEY"])
        .or(stored_provider_api_key(relational, "openai_realtime")?);
    Ok(credential.map(|credential| LlmProviderConfig {
        provider_id: "openai_realtime".into(),
        kind: LlmProviderKind::OpenAiRealtime,
        model: env_or_default("LUMVISE_OPENAI_REALTIME_MODEL", "gpt-realtime"),
        endpoint: env_first(&["LUMVISE_OPENAI_REALTIME_ENDPOINT"]),
        credential: Some(credential),
        completion_concurrency: None,
        spawn: None,
    }))
}

fn cerebras_provider_config(
    relational: &dyn RelationalPersistence,
) -> Result<Option<LlmProviderConfig>, Box<dyn std::error::Error>> {
    let credential = match env_first(&["LUMVISE_CEREBRAS_API_KEY", "CEREBRAS_API_KEY"]) {
        Some(value) => Some(value),
        None => stored_provider_api_key(relational, "cerebras")?,
    };
    let Some(credential) = credential else {
        return Ok(None);
    };
    Ok(Some(LlmProviderConfig {
        provider_id: "cerebras".to_string(),
        kind: LlmProviderKind::Cerebras,
        model: env_or_default("LUMVISE_CEREBRAS_MODEL", "provider-default"),
        endpoint: Some(env_or_default(
            "LUMVISE_CEREBRAS_ENDPOINT",
            "https://api.cerebras.ai/v1",
        )),
        credential: Some(credential),
        completion_concurrency: None,
        spawn: None,
    }))
}

fn openrouter_provider_config(
    relational: &dyn RelationalPersistence,
) -> Result<Option<LlmProviderConfig>, Box<dyn std::error::Error>> {
    let credential = match env_first(&["LUMVISE_OPENROUTER_API_KEY", "OPENROUTER_API_KEY"]) {
        Some(value) => Some(value),
        None => stored_provider_api_key(relational, "openrouter")?,
    };
    let Some(credential) = credential else {
        return Ok(None);
    };
    Ok(Some(LlmProviderConfig {
        provider_id: "openrouter".to_string(),
        kind: LlmProviderKind::OpenRouter,
        model: env_or_default("LUMVISE_OPENROUTER_MODEL", "provider-default"),
        endpoint: Some(env_or_default(
            "LUMVISE_OPENROUTER_ENDPOINT",
            "https://openrouter.ai/api/v1",
        )),
        credential: Some(credential),
        completion_concurrency: None,
        spawn: None,
    }))
}

fn z_ai_provider_config(
    relational: &dyn RelationalPersistence,
) -> Result<Option<LlmProviderConfig>, Box<dyn std::error::Error>> {
    let credential = match env_first(&["LUMVISE_ZAI_API_KEY", "ZAI_API_KEY"]) {
        Some(value) => Some(value),
        None => stored_provider_api_key(relational, "z_ai")?,
    };
    let Some(credential) = credential else {
        return Ok(None);
    };
    Ok(Some(LlmProviderConfig {
        provider_id: "z_ai".to_string(),
        kind: LlmProviderKind::Zai,
        model: env_or_default("LUMVISE_ZAI_MODEL", "provider-default"),
        endpoint: Some(env_or_default(
            "LUMVISE_ZAI_ENDPOINT",
            "https://api.z.ai/api/coding/paas/v4",
        )),
        credential: Some(credential),
        completion_concurrency: None,
        spawn: None,
    }))
}

fn env_or_default(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn env_first(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn assistant_provider_catalog(catalog: &LlmProviderCatalog) -> AssistantProviderCatalog {
    AssistantProviderCatalog {
        providers: catalog
            .providers
            .iter()
            .filter_map(|provider| {
                let source = provider.active_source?;
                let inventory = provider.model_sources.for_source(source)?;
                Some(AssistantProviderOption {
                    id: provider.provider_id.clone(),
                    label: provider.provider_id.clone(),
                    models: inventory
                        .models
                        .iter()
                        .map(|model| AssistantModelOption {
                            id: model.id.clone(),
                            label: model.display_name.clone(),
                        })
                        .collect(),
                    default_model: inventory.default_model.clone(),
                    model_source: match source {
                        LlmModelSource::Api => AssistantModelSource::Api,
                        LlmModelSource::Client => AssistantModelSource::Client,
                    },
                    available: provider.state == LlmProviderAvailability::Available,
                })
            })
            .collect(),
    }
}

#[cfg(all(test, feature = "native-vector"))]
mod tests {
    use crate::AppCore;
    use lumvise_neural_core::managed_models::ManagedModelState;
    use lumvise_neural_core::{LlmConfigurationRequirement, LlmProviderKind};
    use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
    use serde_json::json;
    use std::sync::Arc;

    struct EnvGuard {
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self {
                previous: vec![(key, previous)],
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..) {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    #[test]
    fn custom_openai_candidate_without_endpoint_reports_missing_endpoint() {
        let persistence = Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap());
        let _endpoint = EnvGuard::set("LUMVISE_CUSTOM_OPENAI_ENDPOINT", None);
        let _key = EnvGuard::set("LUMVISE_CUSTOM_OPENAI_API_KEY", None);
        let _model = EnvGuard::set("LUMVISE_CUSTOM_OPENAI_MODEL", None);

        let candidates = super::configured_llm_candidates(persistence.as_ref()).unwrap();
        let candidate = candidates
            .iter()
            .find(|candidate| {
                matches!(
                    candidate,
                    lumvise_neural_core::LlmProviderCandidate::MissingConfiguration {
                        provider_id,
                        ..
                    } if provider_id == "custom_openai"
                ) || matches!(
                    candidate,
                    lumvise_neural_core::LlmProviderCandidate::Configured(config)
                        if config.provider_id == "custom_openai"
                )
            })
            .unwrap();
        assert!(matches!(
            candidate,
            lumvise_neural_core::LlmProviderCandidate::MissingConfiguration {
                kind: LlmProviderKind::OpenAiCompatible,
                missing: LlmConfigurationRequirement::Endpoint,
                ..
            }
        ));
    }

    #[test]
    fn custom_openai_candidate_configures_endpoint_without_credential() {
        let persistence = Arc::new(lumvise_db_core::LocalPersistence::in_memory().unwrap());
        let _endpoint = EnvGuard::set(
            "LUMVISE_CUSTOM_OPENAI_ENDPOINT",
            Some("http://localhost:11434/v1"),
        );
        let _key = EnvGuard::set("LUMVISE_CUSTOM_OPENAI_API_KEY", None);
        let _model = EnvGuard::set("LUMVISE_CUSTOM_OPENAI_MODEL", None);

        let candidates = super::configured_llm_candidates(persistence.as_ref()).unwrap();
        let candidate = candidates
            .into_iter()
            .find_map(|candidate| match candidate {
                lumvise_neural_core::LlmProviderCandidate::Configured(config)
                    if config.provider_id == "custom_openai" =>
                {
                    Some(config)
                }
                _ => None,
            })
            .expect("custom_openai candidate should be configured");

        assert_eq!(candidate.kind, LlmProviderKind::OpenAiCompatible);
        assert_eq!(
            candidate.endpoint.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(candidate.credential, None);
        assert!(candidate.validate().is_ok());
    }

    #[test]
    #[ignore = "requires the cached All-MiniLM model or network access"]
    fn desktop_vectorizer_serves_all_minilm_vectors_through_neural_embed() {
        let app = Arc::new(AppCore::in_memory().expect("in-memory app core"));
        app.install_managed_models()
            .expect("install managed model runtime");
        let manager = app
            .managed_models()
            .expect("managed model manager installed");
        let status = manager
            .select_blocking("vector.all-minilm-l6-v2")
            .expect("select and activate the All-MiniLM vector model");
        assert_eq!(status.state, ManagedModelState::Ready);
        let broker = app
            .default_plugin_host_capability_broker()
            .expect("default host-capability broker");

        let output = broker
            .invoke(HostCapabilityRequest {
                plugin_id: "builtin.semantic".into(),
                invocation_id: "desktop-vectorizer-test".into(),
                call_id: "embed".into(),
                capability_id: "neural.embed".into(),
                required_version: "1".into(),
                input: json!({"texts": ["local semantic search"]}),
            })
            .expect("embed through desktop vectorizer");
        let vector = output["vectors"][0]
            .as_array()
            .expect("one embedded vector");

        assert_eq!(vector.len(), 384);
        assert!(
            vector
                .iter()
                .all(|component| component.as_f64().is_some_and(f64::is_finite))
        );
        assert!(
            vector
                .iter()
                .any(|component| component.as_f64().is_some_and(|value| value != 0.0))
        );
    }
}
