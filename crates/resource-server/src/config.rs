use lumvise_neural_core::{EngineConfig, LlmProviderConfig, LlmProviderKind, SpawnConfig};
use std::{env, net::SocketAddr, path::PathBuf};

use thiserror::Error;
use url::Url;

/// Immutable production configuration for the central resource server.
///
/// All values are deliberately server-owned. In particular, no desktop
/// provider credential or local database path is accepted here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceServerConfig {
    pub bind: SocketAddr,
    pub tls_certificate_path: PathBuf,
    pub tls_key_path: PathBuf,
    pub data_dir: PathBuf,
    pub oidc: ServerOidcConfig,
    pub neural: ServerNeuralConfig,
}

/// Server-owned neural adapter configuration.
///
/// The central server never reads desktop settings or accepts adapter settings
/// over its transport. An absent adapter is intentional and is surfaced as
/// `NOT_CONFIGURED` by readiness.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ServerNeuralConfig {
    pub llm: Option<LlmProviderConfig>,
    pub speech_recognizer: Option<EngineConfig>,
    pub speech_synthesizer: Option<EngineConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerOidcConfig {
    pub issuer_url: Url,
    /// Optional PEM trust anchor for discovery and JWKS HTTPS requests.
    ///
    /// This augments—not replaces—the system root store.
    pub ca_certificate_path: Option<PathBuf>,
    pub audience: String,
    pub tenant_claim: String,
    pub required_scope: String,
}

impl ResourceServerConfig {
    pub fn from_environment() -> Result<Self, ServerConfigError> {
        let bind = required("LUMVISE_RESOURCE_SERVER_BIND")?
            .parse::<SocketAddr>()
            .map_err(|error| ServerConfigError::InvalidBind {
                value: env::var("LUMVISE_RESOURCE_SERVER_BIND").unwrap_or_default(),
                error: error.to_string(),
            })?;
        let issuer_url = Url::parse(&required("LUMVISE_OIDC_ISSUER_URL")?).map_err(|error| {
            ServerConfigError::InvalidUrl {
                variable: "LUMVISE_OIDC_ISSUER_URL",
                error: error.to_string(),
            }
        })?;
        if issuer_url.scheme() != "https" {
            return Err(ServerConfigError::IssuerMustUseHttps);
        }
        let oidc_ca_certificate_path = env::var_os("LUMVISE_OIDC_CA_CERT_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            bind,
            tls_certificate_path: PathBuf::from(required("LUMVISE_RESOURCE_SERVER_TLS_CERT_PATH")?),
            tls_key_path: PathBuf::from(required("LUMVISE_RESOURCE_SERVER_TLS_KEY_PATH")?),
            data_dir: PathBuf::from(required("LUMVISE_RESOURCE_DATA_DIR")?),
            oidc: ServerOidcConfig {
                issuer_url,
                ca_certificate_path: oidc_ca_certificate_path,
                audience: required("LUMVISE_OIDC_AUDIENCE")?,
                tenant_claim: required("LUMVISE_OIDC_TENANT_CLAIM")?,
                required_scope: required("LUMVISE_OIDC_REQUIRED_SCOPE")?,
            },
            neural: ServerNeuralConfig::from_environment()?,
        })
    }
}

impl ServerNeuralConfig {
    /// Loads only server-owned adapter settings. Every adapter is optional so
    /// deployments can deliberately omit a capability and advertise that fact.
    pub fn from_environment() -> Result<Self, ServerConfigError> {
        Ok(Self {
            llm: llm_from_environment()?,
            speech_recognizer: engine_from_environment(
                "LUMVISE_RESOURCE_STT_COMMAND",
                "LUMVISE_RESOURCE_STT_ARGS",
                "LUMVISE_RESOURCE_STT_TIMEOUT_MS",
                "resource-server-stt",
            )?,
            speech_synthesizer: engine_from_environment(
                "LUMVISE_RESOURCE_TTS_COMMAND",
                "LUMVISE_RESOURCE_TTS_ARGS",
                "LUMVISE_RESOURCE_TTS_TIMEOUT_MS",
                "resource-server-tts",
            )?,
        })
    }
}

fn llm_from_environment() -> Result<Option<LlmProviderConfig>, ServerConfigError> {
    let Some(provider_id) = optional("LUMVISE_RESOURCE_LLM_PROVIDER_ID") else {
        return Ok(None);
    };
    let kind = match required("LUMVISE_RESOURCE_LLM_KIND")?
        .to_ascii_lowercase()
        .as_str()
    {
        "cerebras" => LlmProviderKind::Cerebras,
        "claude" => LlmProviderKind::Claude,
        "codex" => LlmProviderKind::Codex,
        "gemini" => LlmProviderKind::Gemini,
        "openai-realtime" | "openai_realtime" => LlmProviderKind::OpenAiRealtime,
        "openrouter" => LlmProviderKind::OpenRouter,
        "zai" | "z-ai" | "z_ai" => LlmProviderKind::Zai,
        "local" => LlmProviderKind::Local,
        value => {
            return Err(ServerConfigError::InvalidNeural {
                variable: "LUMVISE_RESOURCE_LLM_KIND",
                reason: format!("unsupported provider kind {value:?}"),
            });
        }
    };
    let spawn = match optional("LUMVISE_RESOURCE_LLM_COMMAND") {
        Some(command) => Some(SpawnConfig {
            command,
            args: optional("LUMVISE_RESOURCE_LLM_ARGS")
                .map(|args| args.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
            timeout_ms: optional_timeout("LUMVISE_RESOURCE_LLM_TIMEOUT_MS")?,
        }),
        None => None,
    };
    Ok(Some(LlmProviderConfig {
        provider_id,
        kind,
        model: required("LUMVISE_RESOURCE_LLM_MODEL")?,
        endpoint: optional("LUMVISE_RESOURCE_LLM_ENDPOINT"),
        credential: optional("LUMVISE_RESOURCE_LLM_CREDENTIAL"),
        completion_concurrency: optional_usize("LUMVISE_RESOURCE_LLM_COMPLETION_CONCURRENCY")?,
        spawn,
    }))
}

fn engine_from_environment(
    command_variable: &'static str,
    arguments_variable: &'static str,
    timeout_variable: &'static str,
    engine_id: &str,
) -> Result<Option<EngineConfig>, ServerConfigError> {
    let Some(command) = optional(command_variable) else {
        return Ok(None);
    };
    Ok(Some(EngineConfig {
        engine_id: engine_id.into(),
        spawn: SpawnConfig {
            command,
            args: optional(arguments_variable)
                .map(|args| args.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
            timeout_ms: optional_timeout(timeout_variable)?,
        },
        expected_dimensions: None,
    }))
}

fn optional(variable: &'static str) -> Option<String> {
    env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn optional_timeout(variable: &'static str) -> Result<u64, ServerConfigError> {
    optional(variable).map_or(Ok(30_000), |value| {
        value
            .parse()
            .map_err(|error| ServerConfigError::InvalidNeural {
                variable,
                reason: format!("must be a positive millisecond value: {error}"),
            })
    })
}

fn optional_usize(variable: &'static str) -> Result<Option<usize>, ServerConfigError> {
    optional(variable)
        .map(|value| {
            value
                .parse()
                .map_err(|error| ServerConfigError::InvalidNeural {
                    variable,
                    reason: format!("must be a positive integer: {error}"),
                })
        })
        .transpose()
}

fn required(variable: &'static str) -> Result<String, ServerConfigError> {
    env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(ServerConfigError::Missing(variable))
}

#[derive(Debug, Error)]
pub enum ServerConfigError {
    #[error("required server configuration {0} is missing or blank")]
    Missing(&'static str),
    #[error("LUMVISE_RESOURCE_SERVER_BIND is not a host:port socket address ({value:?}): {error}")]
    InvalidBind { value: String, error: String },
    #[error("{variable} is not a valid URL: {error}")]
    InvalidUrl {
        variable: &'static str,
        error: String,
    },
    #[error("LUMVISE_OIDC_ISSUER_URL must use HTTPS")]
    IssuerMustUseHttps,
    #[error("invalid neural configuration {variable}: {reason}")]
    InvalidNeural {
        variable: &'static str,
        reason: String,
    },
}
