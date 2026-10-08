use lumvise_db_core::{CentralizedPersistence, LocalPersistence};
use lumvise_db_core::{RelationalPersistence, SemanticPersistence};

use lumvise_neural_core::{
    CentralizedLlmProvider, CentralizedSpeechRecognizer, CentralizedSpeechSynthesizer,
    ScopedMcpTransport,
};
use lumvise_neural_core::{
    LlmProviderCatalog, LlmProviderRegistry, LlmProviderSync,
    speech::{SpeechRecognizer, SpeechSynthesizer},
};
#[cfg(feature = "assistant-e2e")]
use lumvise_resource_routing::auth::{FixtureBrowserLauncher, RefreshTokenStore};
#[cfg(not(feature = "assistant-e2e"))]
use lumvise_resource_routing::auth::{KeyringCredentialStore, SystemBrowserLauncher};

use lumvise_resource_routing::{
    AuthenticatedFramedClient, BlockingAuthenticatedFramedClient, Http2CentralTransport,
    InvocationControl, ResourceInvocationClient,
    auth::OidcClient,
    protocol::{
        CapabilityReadinessV1, PROTOCOL_MAJOR, PROTOCOL_MINOR, ReadinessRequestV1,
        ResourceCapabilityV1,
    },
};
use lumvise_resource_routing::{ResourcePlacement, ResourceRoutingConfig};

use std::path::PathBuf;
use std::{fmt, sync::Arc};

/// Startup-only resource selection; consumers keep their selected adapters until exit.
/// Private products supply an authenticated transport, never database internals.
/// Example: `AppResourceSelection::RemoteStorage { client, client_instance_id }`.
#[derive(Default)]
pub enum AppResourceSelection {
    #[default]
    Environment,
    Local,
    RemoteStorage {
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: String,
    },
}

/// The immutable capability ownership selected before App Core becomes ready.
///
/// Each field is populated exactly once by [`ResourceRouter::build`]. A route
/// is never retried through its counterpart: a failed selected adapter is a
/// startup failure, not an invitation to silently change data ownership.
#[cfg_attr(test, allow(dead_code))]
pub(super) struct RoutedResources {
    pub(super) llms: LlmProviderRegistry,
    pub(super) llm_catalog: LlmProviderCatalog,
    pub(super) speech_recognizer: Option<Arc<dyn SpeechRecognizer>>,
    pub(super) speech_synthesizer: Option<Arc<dyn SpeechSynthesizer>>,
    pub(super) semantic: Arc<dyn SemanticPersistence>,
    pub(super) relational: Arc<dyn RelationalPersistence>,
}

/// Construction boundary for resource ownership.
///
/// Production composition authenticates and probes centralized capabilities in
/// its factory before passing it here. Keeping that work on this boundary makes
/// selection testable without network or credential side effects and prevents
/// consumers from constructing a second adapter after startup.

trait ResourceAdapterFactory {
    type Error: std::error::Error + Send + Sync + 'static;

    fn internal_llms(&self) -> std::result::Result<LlmProviderSync, Self::Error>;
    fn centralized_llms(&self) -> std::result::Result<LlmProviderSync, Self::Error>;

    /// Internal speech is feature-gated and may deliberately be unavailable.
    fn internal_speech(
        &self,
    ) -> std::result::Result<
        (
            Option<Arc<dyn SpeechRecognizer>>,
            Option<Arc<dyn SpeechSynthesizer>>,
        ),
        Self::Error,
    >;

    /// Central speech is an atomic STT+TTS capability; returning either half is
    /// an invariant violation and fails startup.
    fn centralized_speech(
        &self,
    ) -> std::result::Result<(Arc<dyn SpeechRecognizer>, Arc<dyn SpeechSynthesizer>), Self::Error>;

    fn internal_semantic(&self) -> std::result::Result<Arc<dyn SemanticPersistence>, Self::Error>;
    fn centralized_semantic(
        &self,
    ) -> std::result::Result<Arc<dyn SemanticPersistence>, Self::Error>;
    fn internal_relational(
        &self,
    ) -> std::result::Result<Arc<dyn RelationalPersistence>, Self::Error>;
    fn centralized_relational(
        &self,
    ) -> std::result::Result<Arc<dyn RelationalPersistence>, Self::Error>;
}

#[derive(Debug)]
pub(super) struct ResourceRoutingError {
    capability: &'static str,
    placement: ResourcePlacement,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl ResourceRoutingError {
    fn selected<E>(capability: &'static str, placement: ResourcePlacement, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self {
            capability,
            placement,
            source: Box::new(source),
        }
    }
}

impl fmt::Display for ResourceRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "selected {} route {:?} failed: {}",
            self.capability, self.placement, self.source
        )
    }
}

impl std::error::Error for ResourceRoutingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Selects one and only one adapter for each capability.
pub(super) struct ResourceRouter;
impl ResourceRouter {
    pub(super) fn build_selection(
        selection: AppResourceSelection,
    ) -> Result<(ResourceRoutingConfig, RoutedResources), String> {
        match selection {
            AppResourceSelection::Local => {
                let config = local_config();
                Self::build(config.clone())
                    .map(|resources| (config, resources))
                    .map_err(|error| error.to_string())
            }
            AppResourceSelection::Environment => {
                let config =
                    ResourceRoutingConfig::from_environment().map_err(|error| error.to_string())?;
                Self::build(config.clone())
                    .map(|resources| (config, resources))
                    .map_err(|error| error.to_string())
            }
            AppResourceSelection::RemoteStorage {
                client,
                client_instance_id,
            } => {
                let config = storage_only_config();
                let central = CentralResources::connect(
                    client,
                    client_instance_id,
                    &requested_central_capabilities(&config),
                )
                .map_err(|error| error.to_string())?;
                let factory = ProductionResourceFactory {
                    central: Some(central),
                    local: None,
                };
                Self::build_with_factories(&config, &factory)
                    .map(|resources| (config, resources))
                    .map_err(|error| error.to_string())
            }
        }
    }

    /// Builds the production startup routing once. Configuration is validated
    /// by `ResourceRoutingConfig` before this boundary; central authentication
    /// and readiness are performed once here before any selected adapter is
    /// published to App Core.

    pub(super) fn build(
        config: ResourceRoutingConfig,
    ) -> std::result::Result<RoutedResources, ResourceRoutingError> {
        let factory = ProductionResourceFactory::new(&config)?;
        Self::build_with_factories(&config, &factory)
    }

    fn build_with_factories<F>(
        config: &ResourceRoutingConfig,
        factory: &F,
    ) -> std::result::Result<RoutedResources, ResourceRoutingError>
    where
        F: ResourceAdapterFactory,
    {
        let llm_sync = match config.llm_execution {
            ResourcePlacement::Internal => factory.internal_llms().map_err(|error| {
                ResourceRoutingError::selected("llm_execution", ResourcePlacement::Internal, error)
            })?,
            ResourcePlacement::Centralized => factory.centralized_llms().map_err(|error| {
                ResourceRoutingError::selected(
                    "llm_execution",
                    ResourcePlacement::Centralized,
                    error,
                )
            })?,
        };

        let (speech_recognizer, speech_synthesizer) = match config.speech_inference {
            ResourcePlacement::Internal => factory.internal_speech().map_err(|error| {
                ResourceRoutingError::selected(
                    "speech_inference",
                    ResourcePlacement::Internal,
                    error,
                )
            })?,
            ResourcePlacement::Centralized => {
                let (recognizer, synthesizer) = factory.centralized_speech().map_err(|error| {
                    ResourceRoutingError::selected(
                        "speech_inference",
                        ResourcePlacement::Centralized,
                        error,
                    )
                })?;
                (Some(recognizer), Some(synthesizer))
            }
        };

        let semantic = match config.graph_persistence {
            ResourcePlacement::Internal => factory.internal_semantic().map_err(|error| {
                ResourceRoutingError::selected(
                    "graph_persistence",
                    ResourcePlacement::Internal,
                    error,
                )
            })?,
            ResourcePlacement::Centralized => factory.centralized_semantic().map_err(|error| {
                ResourceRoutingError::selected(
                    "graph_persistence",
                    ResourcePlacement::Centralized,
                    error,
                )
            })?,
        };
        let relational = match config.sql_persistence {
            ResourcePlacement::Internal => factory.internal_relational().map_err(|error| {
                ResourceRoutingError::selected(
                    "sql_persistence",
                    ResourcePlacement::Internal,
                    error,
                )
            })?,
            ResourcePlacement::Centralized => {
                factory.centralized_relational().map_err(|error| {
                    ResourceRoutingError::selected(
                        "sql_persistence",
                        ResourcePlacement::Centralized,
                        error,
                    )
                })?
            }
        };

        Ok(RoutedResources {
            llms: llm_sync.registry,
            llm_catalog: llm_sync.catalog,
            speech_recognizer,
            speech_synthesizer,
            semantic,
            relational,
        })
    }
}

#[derive(Debug)]
struct RouteStartupError(String);

impl fmt::Display for RouteStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RouteStartupError {}

struct CentralResources {
    client: Arc<dyn ResourceInvocationClient>,
    client_instance_id: String,
    persistence: Arc<CentralizedPersistence>,
    llm_descriptors: Vec<lumvise_resource_routing::protocol::LlmProviderDescriptorV1>,
}

impl CentralResources {
    fn connect(
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: String,
        requested: &[ResourceCapabilityV1],
    ) -> Result<Self, ResourceRoutingError> {
        if client_instance_id.trim().is_empty() {
            return Err(central_readiness_error(RouteStartupError(
                "empty client_instance_id; expected stable client identity".into(),
            )));
        }
        let control = InvocationControl::sixty_seconds();
        let readiness = client
            .readiness(
                &readiness_request(&client_instance_id, requested, &control),
                &control,
            )
            .map_err(central_readiness_error)?;
        ensure_central_readiness(requested, &readiness.capabilities)
            .map_err(central_readiness_error)?;
        let persistence = Arc::new(CentralizedPersistence::new(
            Arc::clone(&client),
            client_instance_id.clone(),
        ));
        Ok(Self {
            client,
            client_instance_id,
            persistence,
            llm_descriptors: readiness.llm_providers,
        })
    }
}

fn central_readiness_error(
    error: impl std::error::Error + Send + Sync + 'static,
) -> ResourceRoutingError {
    ResourceRoutingError::selected("central_readiness", ResourcePlacement::Centralized, error)
}

fn readiness_request(
    client: &str,
    requested: &[ResourceCapabilityV1],
    control: &InvocationControl,
) -> ReadinessRequestV1 {
    ReadinessRequestV1 {
        supported_majors: vec![PROTOCOL_MAJOR],
        supported_minors: vec![PROTOCOL_MINOR],
        deadline_unix_ms: control.deadline_unix_ms(),
        client_instance_id: client.into(),
        requested_capabilities: requested
            .iter()
            .map(|capability| *capability as i32)
            .collect(),
    }
}

fn storage_only_config() -> ResourceRoutingConfig {
    ResourceRoutingConfig {
        graph_persistence: ResourcePlacement::Centralized,
        sql_persistence: ResourcePlacement::Centralized,
        llm_execution: ResourcePlacement::Internal,
        speech_inference: ResourcePlacement::Internal,
        central: None,
    }
}

fn local_config() -> ResourceRoutingConfig {
    ResourceRoutingConfig {
        graph_persistence: ResourcePlacement::Internal,
        sql_persistence: ResourcePlacement::Internal,
        llm_execution: ResourcePlacement::Internal,
        speech_inference: ResourcePlacement::Internal,
        central: None,
    }
}

/// The sole production construction site for local persistence and the
/// authenticated central client.
struct ProductionResourceFactory {
    central: Option<CentralResources>,
    local: Option<Arc<LocalPersistence>>,
}

impl ProductionResourceFactory {
    fn new(config: &ResourceRoutingConfig) -> std::result::Result<Self, ResourceRoutingError> {
        let requested = requested_central_capabilities(config);
        let central = if requested.is_empty() {
            None
        } else {
            let central_config = config.central.as_ref().ok_or_else(|| {
                ResourceRoutingError::selected(
                    "central_resources",
                    ResourcePlacement::Centralized,
                    RouteStartupError(
                        "central route selected without central configuration".into(),
                    ),
                )
            })?;
            let control = InvocationControl::sixty_seconds();
            #[cfg(feature = "assistant-e2e")]
            let credential_store = Arc::new(RefreshTokenStore::default());
            #[cfg(not(feature = "assistant-e2e"))]
            let credential_store = Arc::new(KeyringCredentialStore);
            let oidc = OidcClient::new(central_config.oidc.clone(), credential_store);
            let token = match oidc.stored_refresh_token().map_err(|error| {
                ResourceRoutingError::selected(
                    "central_authentication",
                    ResourcePlacement::Centralized,
                    error,
                )
            })? {
                Some(_) => oidc.refresh_access_token(&control),
                None => {
                    #[cfg(feature = "assistant-e2e")]
                    {
                        let ca_certificate_path = central_config
                            .oidc
                            .ca_certificate_path
                            .as_deref()
                            .ok_or_else(|| {
                                ResourceRoutingError::selected(
                                    "central_authentication",
                                    ResourcePlacement::Centralized,
                                    RouteStartupError(
                                        "assistant-e2e OIDC fixture requires a CA certificate"
                                            .into(),
                                    ),
                                )
                            })?;
                        let launcher = FixtureBrowserLauncher::new(ca_certificate_path);
                        oidc.authenticate(&launcher, &control)
                    }
                    #[cfg(not(feature = "assistant-e2e"))]
                    {
                        oidc.authenticate(&SystemBrowserLauncher, &control)
                    }
                }
            }
            .map_err(|error| {
                ResourceRoutingError::selected(
                    "central_authentication",
                    ResourcePlacement::Centralized,
                    error,
                )
            })?;
            let transport =
                Http2CentralTransport::from_config(central_config).map_err(|error| {
                    ResourceRoutingError::selected(
                        "central_transport",
                        ResourcePlacement::Centralized,
                        error,
                    )
                })?;
            let client: Arc<dyn ResourceInvocationClient> = Arc::new(
                BlockingAuthenticatedFramedClient::new(AuthenticatedFramedClient::new(
                    Arc::new(transport),
                    token.value,
                ))
                .map_err(|error| {
                    ResourceRoutingError::selected(
                        "central_transport",
                        ResourcePlacement::Centralized,
                        error,
                    )
                })?,
            );
            Some(CentralResources::connect(
                client,
                uuid::Uuid::new_v4().to_string(),
                &requested,
            )?)
        };
        let database_path = configured_database_path();
        let local = requires_local_persistence(config)
            .then(|| {
                LocalPersistence::open(&database_path)
                    .map(Arc::new)
                    .map_err(|error| {
                        ResourceRoutingError::selected(
                            "local_persistence",
                            ResourcePlacement::Internal,
                            RouteStartupError(format!(
                                "constructing shared local persistence adapter: {error}"
                            )),
                        )
                    })
            })
            .transpose()?;
        Ok(Self { central, local })
    }

    fn central(
        &self,
        capability: &'static str,
    ) -> std::result::Result<&CentralResources, RouteStartupError> {
        self.central.as_ref().ok_or_else(|| {
            RouteStartupError(format!(
                "selected centralized {capability} route has no authenticated ready client"
            ))
        })
    }

    fn local_persistence(&self) -> std::result::Result<Arc<LocalPersistence>, RouteStartupError> {
        self.local.as_ref().map(Arc::clone).ok_or_else(|| {
            RouteStartupError("selected local persistence adapter is unavailable".into())
        })
    }
}

impl ResourceAdapterFactory for ProductionResourceFactory {
    type Error = RouteStartupError;

    fn internal_llms(&self) -> std::result::Result<LlmProviderSync, Self::Error> {
        // Provider discovery makes per-provider network/subprocess calls that
        // can take up to the configured request timeout; running it here
        // blocked the Workspace window from opening until every candidate
        // settled. The desktop executor installs configured providers after
        // the selected relational adapter is available, then runs discovery
        // in the background without delaying their first request.
        Ok(LlmProviderSync {
            registry: LlmProviderRegistry::empty(),
            catalog: LlmProviderCatalog::default(),
        })
    }

    fn centralized_llms(&self) -> std::result::Result<LlmProviderSync, Self::Error> {
        let central = self.central("llm_execution")?;
        let registry = CentralizedLlmProvider::registry_from_readiness(
            central.llm_descriptors.clone(),
            Arc::clone(&central.client),
            central.client_instance_id.clone(),
            Arc::new(ScopedMcpTransport::default()),
        )
        .map_err(|error| {
            RouteStartupError(format!(
                "constructing selected centralized LLM adapter: {error}"
            ))
        })?;
        Ok(LlmProviderSync {
            registry,
            catalog: LlmProviderCatalog::default(),
        })
    }

    fn internal_speech(
        &self,
    ) -> std::result::Result<
        (
            Option<Arc<dyn SpeechRecognizer>>,
            Option<Arc<dyn SpeechSynthesizer>>,
        ),
        Self::Error,
    > {
        Ok((None, None))
    }

    fn centralized_speech(
        &self,
    ) -> std::result::Result<(Arc<dyn SpeechRecognizer>, Arc<dyn SpeechSynthesizer>), Self::Error>
    {
        let central = self.central("speech_inference")?;
        Ok((
            Arc::new(CentralizedSpeechRecognizer::new(
                Arc::clone(&central.client),
                central.client_instance_id.clone(),
            )),
            Arc::new(CentralizedSpeechSynthesizer::new(
                Arc::clone(&central.client),
                central.client_instance_id.clone(),
            )),
        ))
    }

    fn internal_semantic(&self) -> std::result::Result<Arc<dyn SemanticPersistence>, Self::Error> {
        let semantic: Arc<dyn SemanticPersistence> = self.local_persistence()?;
        Ok(semantic)
    }

    fn centralized_semantic(
        &self,
    ) -> std::result::Result<Arc<dyn SemanticPersistence>, Self::Error> {
        let central = self.central("graph_persistence")?;
        let semantic: Arc<dyn SemanticPersistence> = central.persistence.clone();
        Ok(semantic)
    }

    fn internal_relational(
        &self,
    ) -> std::result::Result<Arc<dyn RelationalPersistence>, Self::Error> {
        let relational: Arc<dyn RelationalPersistence> = self.local_persistence()?;
        Ok(relational)
    }

    fn centralized_relational(
        &self,
    ) -> std::result::Result<Arc<dyn RelationalPersistence>, Self::Error> {
        let central = self.central("sql_persistence")?;
        let relational: Arc<dyn RelationalPersistence> = central.persistence.clone();
        Ok(relational)
    }
}

fn configured_database_path() -> PathBuf {
    std::env::var_os("LUMVISE_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(default_database_path)
}

fn default_database_path() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".lumvise")
        .join("database")
        .join("lumvise.db")
}

fn requires_local_persistence(config: &ResourceRoutingConfig) -> bool {
    config.graph_persistence == ResourcePlacement::Internal
        || config.sql_persistence == ResourcePlacement::Internal
}

fn requested_central_capabilities(config: &ResourceRoutingConfig) -> Vec<ResourceCapabilityV1> {
    [
        (config.llm_execution, ResourceCapabilityV1::LlmExecution),
        (
            config.speech_inference,
            ResourceCapabilityV1::SpeechInference,
        ),
        (
            config.graph_persistence,
            ResourceCapabilityV1::GraphPersistence,
        ),
        (config.sql_persistence, ResourceCapabilityV1::SqlPersistence),
    ]
    .into_iter()
    .filter_map(|(placement, capability)| {
        (placement == ResourcePlacement::Centralized).then_some(capability)
    })
    .collect()
}

fn ensure_central_readiness(
    requested: &[ResourceCapabilityV1],
    entries: &[lumvise_resource_routing::protocol::CapabilityReadinessEntryV1],
) -> std::result::Result<(), RouteStartupError> {
    for capability in requested {
        let ready = entries.iter().any(|entry| {
            entry.capability == *capability as i32
                && entry.status == CapabilityReadinessV1::Ready as i32
        });
        if !ready {
            return Err(RouteStartupError(format!(
                "selected centralized {} route is not ready",
                capability_name(*capability)
            )));
        }
    }
    Ok(())
}

fn capability_name(capability: ResourceCapabilityV1) -> &'static str {
    match capability {
        ResourceCapabilityV1::LlmExecution => "llm_execution",
        ResourceCapabilityV1::SpeechInference => "speech_inference",
        ResourceCapabilityV1::GraphPersistence => "graph_persistence",
        ResourceCapabilityV1::SqlPersistence => "sql_persistence",
    }
}

#[cfg(test)]
#[path = "resource_routing/tests.rs"]
mod tests;
