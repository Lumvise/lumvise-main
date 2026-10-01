use crate::config::{LlmProviderConfig, LlmProviderKind, SpawnConfig};
use crate::error::{NeuralError, Result};
use crate::llm_providers::adapter::{
    LlmAdapterPreferences, LlmProviderAdapterPlan, LlmTransportKind, ResolvedLlmProviderAdapterPlan,
};
use crate::llm_providers::command_runner::{
    LlmCommandTransport, ProviderCommandOutput, ProviderCommandTransport,
};
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest};
use crate::llm_providers::model_catalog::{
    LlmModelSource, ProviderModelCatalog, ProviderModelSources,
};
use crate::llm_providers::registry::{LlmProviderRegistry, ResolvedProviderConfig};
use crate::llm_providers::{LlmMessage, LlmRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum LlmProviderCandidate {
    Configured(LlmProviderConfig),
    MissingConfiguration {
        provider_id: String,
        kind: LlmProviderKind,
        missing: LlmConfigurationRequirement,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmConfigurationRequirement {
    Credential,
    Endpoint,
    Executable,
    Configuration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum LlmProviderAvailability {
    MissingConfiguration {
        requirement: LlmConfigurationRequirement,
    },
    DiscoveryFailed {
        message: String,
    },
    InvocationFailed {
        message: String,
    },
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmModelDescriptor {
    pub id: String,
    pub display_name: String,
}

/// Availability and catalog content are independent: every status carries the
/// complete source-aware catalog even when no usable transport is configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProviderStatus {
    pub provider_id: String,
    pub kind: LlmProviderKind,
    pub state: LlmProviderAvailability,
    pub model_sources: ProviderModelSources,
    pub active_source: Option<LlmModelSource>,
    pub selected_transport: Option<LlmTransportKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProviderCatalog {
    pub providers: Vec<LlmProviderStatus>,
}

impl LlmProviderCatalog {
    pub fn available_provider(&self, provider_id: &str) -> Option<&LlmProviderStatus> {
        self.providers.iter().find(|provider| {
            provider.provider_id == provider_id
                && provider.state == LlmProviderAvailability::Available
        })
    }

    /// Tests membership against the active source, never the other source.
    pub fn contains_model(&self, provider_id: &str, model: &str) -> bool {
        self.providers
            .iter()
            .find(|provider| provider.provider_id == provider_id)
            .and_then(|provider| provider.active_source.map(|source| (provider, source)))
            .and_then(|(provider, source)| provider.model_sources.for_source(source))
            .is_some_and(|inventory| {
                inventory
                    .models
                    .iter()
                    .any(|candidate| candidate.id == model)
            })
    }
}

pub struct LlmProviderSync {
    pub registry: LlmProviderRegistry,
    pub catalog: LlmProviderCatalog,
}

/// Resolves the construction transport before discovery, so a sync performs at
/// most one compatible-source inventory query and one selected-model probe.
pub struct LlmProviderSynchronizer {
    http: Arc<dyn LlmHttpClient>,
    commands: Arc<dyn LlmCommandTransport>,
    catalog: ProviderModelCatalog,
}

impl LlmProviderSynchronizer {
    pub fn new(
        http: Arc<dyn LlmHttpClient>,
        commands: Arc<dyn LlmCommandTransport>,
        catalog: ProviderModelCatalog,
    ) -> Self {
        Self {
            http,
            commands,
            catalog,
        }
    }

    pub fn production(http: Arc<dyn LlmHttpClient>) -> Result<Self> {
        Ok(Self::new(
            http,
            Arc::new(ProviderCommandTransport),
            ProviderModelCatalog::configured()?,
        ))
    }

    /// Runs every candidate's discovery+probe concurrently (one thread per
    /// provider) instead of sequentially, so total wall time is bounded by
    /// the single slowest provider instead of their sum. CLI providers
    /// (Codex/Claude/Gemini) each spawn a real subprocess for discovery and
    /// a real completion for the probe; doing that six times back-to-back
    /// made desktop startup take minutes under load. Output order and every
    /// per-candidate error-handling branch are unchanged from the prior
    /// sequential implementation; only the execution strategy changed.
    pub fn sync(&self, candidates: Vec<LlmProviderCandidate>) -> Result<LlmProviderSync> {
        let results: Vec<(LlmProviderStatus, Option<ResolvedProviderConfig>)> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = candidates
                    .into_iter()
                    .map(|candidate| {
                        let (provider_id, kind) = Self::candidate_identity(&candidate);
                        let handle = scope.spawn(|| self.sync_one(candidate));
                        (provider_id, kind, handle)
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|(provider_id, kind, handle)| {
                        handle.join().unwrap_or_else(|_| {
                            let model_sources = self.catalog.sources(kind).clone();
                            let active_source = ui_source(&model_sources);
                            (
                                status(
                                    provider_id,
                                    kind,
                                    LlmProviderAvailability::DiscoveryFailed {
                                        message: "provider sync worker panicked".to_string(),
                                    },
                                    model_sources,
                                    active_source,
                                    None,
                                ),
                                None,
                            )
                        })
                    })
                    .collect()
            });

        let mut statuses = Vec::with_capacity(results.len());
        let mut selectable = Vec::new();
        for (status, resolved) in results {
            statuses.push(status);
            if let Some(resolved) = resolved {
                selectable.push(resolved);
            }
        }

        Ok(LlmProviderSync {
            registry: LlmProviderRegistry::from_resolved_configs(selectable, self.http.clone())?,
            catalog: LlmProviderCatalog {
                providers: statuses,
            },
        })
    }

    fn sync_one(
        &self,
        candidate: LlmProviderCandidate,
    ) -> (LlmProviderStatus, Option<ResolvedProviderConfig>) {
        match candidate {
            LlmProviderCandidate::MissingConfiguration {
                provider_id,
                kind,
                missing,
            } => {
                let model_sources = self.catalog.sources(kind).clone();
                let active_source = ui_source(&model_sources);
                (
                    status(
                        provider_id,
                        kind,
                        LlmProviderAvailability::MissingConfiguration {
                            requirement: missing,
                        },
                        model_sources,
                        active_source,
                        None,
                    ),
                    None,
                )
            }
            LlmProviderCandidate::Configured(config) => {
                let provider_id = config.provider_id.clone();
                let kind = config.kind;
                let declared_sources = self.catalog.sources(kind).clone();
                let fallback_source = ui_source(&declared_sources);
                if let Err(error) = config.validate() {
                    return (
                        status(
                            provider_id,
                            kind,
                            LlmProviderAvailability::MissingConfiguration {
                                requirement: missing_requirement(&config, &error),
                            },
                            declared_sources,
                            fallback_source,
                            None,
                        ),
                        None,
                    );
                }
                let initial_plan =
                    LlmProviderAdapterPlan::new(config.clone(), LlmAdapterPreferences::default())
                        .resolve(&adapter_probe_request(&config));
                let resolved_plan = match initial_plan {
                    Ok(plan) => plan,
                    Err(error) => {
                        return (
                            status(
                                provider_id,
                                kind,
                                LlmProviderAvailability::MissingConfiguration {
                                    requirement: missing_requirement(&config, &error),
                                },
                                declared_sources,
                                fallback_source,
                                None,
                            ),
                            None,
                        );
                    }
                };
                let transport = resolved_plan.selected_transport;
                let source = LlmModelSource::for_transport(transport);
                if declared_sources.for_source(source).is_none() {
                    return (
                        status(
                            provider_id,
                            kind,
                            LlmProviderAvailability::DiscoveryFailed {
                                message: format!(
                                    "selected {transport:?} transport has no {source:?} catalog source"
                                ),
                            },
                            declared_sources,
                            Some(source),
                            Some(transport),
                        ),
                        None,
                    );
                }
                let discovered = self.discover(&config, transport);
                let discovery_error = discovered.as_ref().err().map(public_error);
                let resolved_models =
                    match self
                        .catalog
                        .resolve(kind, source, discovered.ok(), &config.model)
                    {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            return (
                                status(
                                    provider_id,
                                    kind,
                                    LlmProviderAvailability::DiscoveryFailed {
                                        message: public_error(&error),
                                    },
                                    declared_sources,
                                    Some(source),
                                    Some(transport),
                                ),
                                None,
                            );
                        }
                    };
                let mut verified_config = config;
                verified_config.model = resolved_models.selected_model.clone();
                let verified_plan = ResolvedLlmProviderAdapterPlan {
                    plan: LlmProviderAdapterPlan::new(
                        verified_config.clone(),
                        LlmAdapterPreferences::default(),
                    ),
                    selected_transport: transport,
                };
                let resolved_config = ResolvedProviderConfig {
                    config: verified_config.clone(),
                    resolved_plan: verified_plan,
                    model_sources: resolved_models.sources.clone(),
                };
                match self.probe(&resolved_config, &resolved_models.selected_model) {
                    Ok(()) => (
                        status(
                            provider_id,
                            kind,
                            LlmProviderAvailability::Available,
                            resolved_models.sources,
                            Some(source),
                            Some(transport),
                        ),
                        Some(resolved_config),
                    ),
                    Err(error) => (
                        status(
                            provider_id,
                            kind,
                            LlmProviderAvailability::InvocationFailed {
                                message: match discovery_error {
                                    Some(discovery_error) => format!(
                                        "{}; model discovery also failed: {discovery_error}",
                                        public_error(&error)
                                    ),
                                    None => public_error(&error),
                                },
                            },
                            resolved_models.sources,
                            Some(source),
                            Some(transport),
                        ),
                        Some(resolved_config),
                    ),
                }
            }
        }
    }

    fn candidate_identity(candidate: &LlmProviderCandidate) -> (String, LlmProviderKind) {
        match candidate {
            LlmProviderCandidate::MissingConfiguration {
                provider_id, kind, ..
            } => (provider_id.clone(), *kind),
            LlmProviderCandidate::Configured(config) => (config.provider_id.clone(), config.kind),
        }
    }

    fn discover(
        &self,
        config: &LlmProviderConfig,
        transport: LlmTransportKind,
    ) -> Result<Vec<LlmModelDescriptor>> {
        match LlmModelSource::for_transport(transport) {
            LlmModelSource::Api => self.discover_api(config),
            LlmModelSource::Client => self.discover_cli(config),
        }
    }

    fn discover_api(&self, config: &LlmProviderConfig) -> Result<Vec<LlmModelDescriptor>> {
        let response = self.http.get_json(&LlmHttpRequest {
            timeout: None,
            endpoint: inventory_endpoint(config)?,
            credential: config.credential.clone().unwrap_or_default(),
            headers: discovery_headers(config),
            payload: Value::Null,
        })?;
        let models = parse_inventory(config.kind, response);
        if models.is_empty() {
            return Err(NeuralError::ProviderFailed {
                provider_id: config.provider_id.clone(),
                message: "provider returned no usable models".to_string(),
            });
        }
        Ok(models)
    }

    fn discover_cli(&self, config: &LlmProviderConfig) -> Result<Vec<LlmModelDescriptor>> {
        let args = cli_inventory_args(config.kind);
        if args.is_empty() {
            return Err(NeuralError::ProviderFailed {
                provider_id: config.provider_id.clone(),
                message: "provider CLI has no non-interactive model listing command".to_string(),
            });
        }
        let spawn = required_spawn(config)?;
        let output = self.commands.run(&spawn, args, None)?;
        let models = parse_inventory(config.kind, parse_json(&output, &config.provider_id)?);
        if models.is_empty() {
            return Err(NeuralError::ProviderFailed {
                provider_id: config.provider_id.clone(),
                message: "CLI did not report concrete model identifiers".to_string(),
            });
        }
        Ok(models)
    }

    fn probe(&self, resolved: &ResolvedProviderConfig, model: &str) -> Result<()> {
        match LlmModelSource::for_transport(resolved.resolved_plan.selected_transport) {
            LlmModelSource::Client => {
                let output = self.commands.run(
                    &required_spawn(&resolved.config)?,
                    cli_probe_args(resolved.config.kind, model),
                    None,
                )?;
                if output.stdout.trim().is_empty() {
                    return Err(NeuralError::ProviderFailed {
                        provider_id: resolved.config.provider_id.clone(),
                        message: "CLI probe returned no final text".to_string(),
                    });
                }
                Ok(())
            }
            LlmModelSource::Api => {
                let registry = LlmProviderRegistry::from_resolved_configs(
                    vec![ResolvedProviderConfig {
                        config: resolved.config.clone(),
                        resolved_plan: resolved.resolved_plan.clone(),
                        model_sources: resolved.model_sources.clone(),
                    }],
                    self.http.clone(),
                )?;
                let response = registry.complete(
                    &resolved.config.provider_id,
                    &LlmRequest {
                        options: Default::default(),
                        messages: vec![LlmMessage {
                            role: "user".to_string(),
                            content: "Reply exactly OK".to_string(),
                        }],
                        stream: false,
                        provider_id: Some(resolved.config.provider_id.clone()),
                        model: Some(model.to_string()),
                        conversation_id: None,
                        provider_session_id: None,
                        mcp_servers: Vec::new(),
                        modality_inputs: Vec::new(),
                    },
                )?;
                if response.content.trim().is_empty() {
                    return Err(NeuralError::ProviderFailed {
                        provider_id: resolved.config.provider_id.clone(),
                        message: "provider probe returned no final text".to_string(),
                    });
                }
                Ok(())
            }
        }
    }
}

fn ui_source(sources: &ProviderModelSources) -> Option<LlmModelSource> {
    match (&sources.api, &sources.client) {
        (Some(_), None) => Some(LlmModelSource::Api),
        (None, Some(_)) => Some(LlmModelSource::Client),
        // Dual providers preserve the established spawn-backed client default
        // until a concrete transport is negotiated.
        (Some(_), Some(_)) => Some(LlmModelSource::Client),
        (None, None) => None,
    }
}

fn status(
    provider_id: String,
    kind: LlmProviderKind,
    state: LlmProviderAvailability,
    model_sources: ProviderModelSources,
    active_source: Option<LlmModelSource>,
    selected_transport: Option<LlmTransportKind>,
) -> LlmProviderStatus {
    LlmProviderStatus {
        provider_id,
        kind,
        state,
        model_sources,
        active_source,
        selected_transport,
    }
}

fn adapter_probe_request(config: &LlmProviderConfig) -> LlmRequest {
    LlmRequest {
        options: Default::default(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: "adapter probe".to_string(),
        }],
        stream: false,
        provider_id: Some(config.provider_id.clone()),
        model: None,
        conversation_id: None,
        provider_session_id: None,
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    }
}

fn required_spawn(config: &LlmProviderConfig) -> Result<SpawnConfig> {
    config
        .spawn
        .clone()
        .ok_or_else(|| NeuralError::MissingValue {
            value: config.provider_id.clone(),
            expected: "provider CLI spawn config".to_string(),
        })
}

fn missing_requirement(
    config: &LlmProviderConfig,
    _error: &NeuralError,
) -> LlmConfigurationRequirement {
    // The credential is optional for user-owned OpenAI-compatible endpoints;
    // the endpoint is the required piece of configuration.
    if config.kind == LlmProviderKind::OpenAiCompatible
        && config
            .endpoint
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        LlmConfigurationRequirement::Endpoint
    } else if config.spawn.is_none()
        && config
            .credential
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        LlmConfigurationRequirement::Credential
    } else if config.spawn.is_none()
        && config
            .endpoint
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        LlmConfigurationRequirement::Endpoint
    } else if config.spawn.is_none() {
        LlmConfigurationRequirement::Configuration
    } else {
        LlmConfigurationRequirement::Executable
    }
}

fn inventory_endpoint(config: &LlmProviderConfig) -> Result<String> {
    let base = config
        .endpoint
        .as_deref()
        .ok_or_else(|| NeuralError::MissingValue {
            value: config.provider_id.clone(),
            expected: "provider model inventory endpoint".to_string(),
        })?
        .trim_end_matches('/');
    Ok(format!("{base}/models"))
}

fn discovery_headers(config: &LlmProviderConfig) -> BTreeMap<String, String> {
    match config.kind {
        LlmProviderKind::Claude => BTreeMap::from([
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            (
                "x-api-key".to_string(),
                config.credential.clone().unwrap_or_default(),
            ),
        ]),
        LlmProviderKind::Gemini => BTreeMap::from([(
            "x-goog-api-key".to_string(),
            config.credential.clone().unwrap_or_default(),
        )]),
        _ => BTreeMap::new(),
    }
}

fn cli_inventory_args(kind: LlmProviderKind) -> Vec<String> {
    match kind {
        LlmProviderKind::Codex => vec!["debug".to_string(), "models".to_string()],
        // Claude Code and Gemini CLI have no non-interactive model-listing
        // command (verified against Claude Code 2.1.205 and Gemini CLI
        // 0.46.0's `--help` command lists - neither exposes a `models`
        // subcommand). Discovery is skipped; `ProviderModelCatalog`'s
        // declared static entries (see provider-models.toml) are used as-is.
        _ => Vec::new(),
    }
}

fn cli_probe_args(kind: LlmProviderKind, model: &str) -> Vec<String> {
    let prompt = "Reply exactly OK".to_string();
    match kind {
        LlmProviderKind::Codex => vec![
            "exec".to_string(),
            "-c".to_string(),
            super::codex::APP_OWNED_CODEX_NOTIFY.to_string(),
            "--skip-git-repo-check".to_string(),
            "--sandbox".to_string(),
            "read-only".to_string(),
            "--json".to_string(),
            "--model".to_string(),
            model.to_string(),
            prompt,
        ],
        LlmProviderKind::Claude => {
            let mut args = vec![
                "-p".to_string(),
                prompt,
                "--output-format".to_string(),
                "json".to_string(),
                "--model".to_string(),
                model.to_string(),
            ];
            args.extend(super::claude::isolated_mcp_args(&[]));
            args
        }
        LlmProviderKind::Gemini => vec![
            "--prompt".to_string(),
            prompt,
            "--output-format".to_string(),
            "json".to_string(),
            "--model".to_string(),
            model.to_string(),
        ],
        _ => Vec::new(),
    }
}

fn parse_json(output: &ProviderCommandOutput, provider_id: &str) -> Result<Value> {
    serde_json::from_str(&output.stdout).map_err(|_| NeuralError::ProviderFailed {
        provider_id: provider_id.to_string(),
        message: "CLI model inventory was not valid JSON".to_string(),
    })
}

fn parse_inventory(kind: LlmProviderKind, value: Value) -> Vec<LlmModelDescriptor> {
    let entries = match kind {
        LlmProviderKind::Gemini => value
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => value
            .get("data")
            .or_else(|| value.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };
    let mut ids = BTreeSet::new();
    for entry in entries {
        if kind == LlmProviderKind::Gemini
            && !entry
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_some_and(|methods| {
                    methods
                        .iter()
                        .any(|method| method.as_str() == Some("generateContent"))
                })
        {
            continue;
        }
        if let Some(id) = entry
            .get("id")
            .or_else(|| entry.get("slug"))
            .or_else(|| entry.get("name"))
            .and_then(Value::as_str)
            .map(|id| id.strip_prefix("models/").unwrap_or(id).trim().to_string())
            .filter(|id| !id.is_empty())
        {
            ids.insert(id);
        }
    }
    ids.into_iter()
        .map(|id| LlmModelDescriptor {
            display_name: id.clone(),
            id,
        })
        .collect()
}

fn public_error(error: &NeuralError) -> String {
    match error {
        NeuralError::ProviderFailed { message, .. } => message.clone(),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
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
                    serde_json::json!({"models":[{"id":"client-live"}]}).to_string()
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
                .any(|model| model.id == "gpt-5.4")
        );
        assert_eq!(*http.calls.lock(), 0);
        assert_eq!(commands.calls.lock().len(), 2);
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
}
