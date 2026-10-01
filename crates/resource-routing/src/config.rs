use std::{env, path::PathBuf};

use thiserror::Error;
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResourcePlacement {
    #[default]
    Internal,
    Centralized,
}

impl ResourcePlacement {
    fn from_environment(name: &'static str) -> Result<Self, RoutingConfigError> {
        match env::var(name) {
            Err(env::VarError::NotPresent) => Ok(Self::Internal),
            Err(env::VarError::NotUnicode(_)) => Err(RoutingConfigError::InvalidPlacement {
                variable: name,
                value: "<non-unicode>".to_owned(),
            }),
            Ok(value) if value.trim().is_empty() => Ok(Self::Internal),
            Ok(value) if value == "internal" => Ok(Self::Internal),
            Ok(value) if value == "centralized" => Ok(Self::Centralized),
            Ok(value) => Err(RoutingConfigError::InvalidPlacement {
                variable: name,
                value,
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcClientConfig {
    pub issuer_url: Url,
    pub client_id: String,
    pub audience: String,
    pub scopes: Vec<String>,
    pub ca_certificate_path: Option<PathBuf>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CentralServerConfig {
    pub url: Url,
    pub ca_certificate_path: Option<PathBuf>,
    pub oidc: OidcClientConfig,
}

/// Immutable startup-only placement selection. It has no mutation API by
/// design; callers construct it once before publishing application readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceRoutingConfig {
    pub llm_execution: ResourcePlacement,
    pub speech_inference: ResourcePlacement,
    pub graph_persistence: ResourcePlacement,
    pub sql_persistence: ResourcePlacement,
    pub central: Option<CentralServerConfig>,
}

impl ResourceRoutingConfig {
    pub fn from_environment() -> Result<Self, RoutingConfigError> {
        let llm_execution = ResourcePlacement::from_environment("LUMVISE_ROUTE_LLM_EXECUTION")?;
        let speech_inference =
            ResourcePlacement::from_environment("LUMVISE_ROUTE_SPEECH_INFERENCE")?;
        let graph_persistence =
            ResourcePlacement::from_environment("LUMVISE_ROUTE_GRAPH_PERSISTENCE")?;
        let sql_persistence = ResourcePlacement::from_environment("LUMVISE_ROUTE_SQL_PERSISTENCE")?;
        let uses_central = [
            llm_execution,
            speech_inference,
            graph_persistence,
            sql_persistence,
        ]
        .into_iter()
        .any(|placement| placement == ResourcePlacement::Centralized);

        let central = uses_central
            .then(Self::central_from_environment)
            .transpose()?;
        Ok(Self {
            llm_execution,
            speech_inference,
            graph_persistence,
            sql_persistence,
            central,
        })
    }

    pub fn uses_centralized_resources(&self) -> bool {
        self.central.is_some()
    }

    fn central_from_environment() -> Result<CentralServerConfig, RoutingConfigError> {
        let url = required_url("LUMVISE_CENTRAL_SERVER_URL")?;
        if url.scheme() != "https" {
            return Err(RoutingConfigError::HttpsRequired {
                variable: "LUMVISE_CENTRAL_SERVER_URL",
                value: url.to_string(),
            });
        }
        let issuer_url = required_url("LUMVISE_OIDC_ISSUER_URL")?;
        if issuer_url.scheme() != "https" {
            return Err(RoutingConfigError::HttpsRequired {
                variable: "LUMVISE_OIDC_ISSUER_URL",
                value: issuer_url.to_string(),
            });
        }
        let client_id = required_value("LUMVISE_OIDC_CLIENT_ID")?;
        let audience = required_value("LUMVISE_OIDC_AUDIENCE")?;
        let scopes = required_value("LUMVISE_OIDC_SCOPES")?
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if scopes.is_empty() {
            return Err(RoutingConfigError::MissingRequired {
                variable: "LUMVISE_OIDC_SCOPES",
            });
        }
        let ca_certificate_path = match env::var("LUMVISE_CENTRAL_SERVER_CA_CERT_PATH") {
            Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value)),
            Ok(_) | Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(RoutingConfigError::InvalidUnicode {
                    variable: "LUMVISE_CENTRAL_SERVER_CA_CERT_PATH",
                });
            }
        };
        let oidc = OidcClientConfig {
            issuer_url,
            client_id,
            audience,
            scopes,
            ca_certificate_path: ca_certificate_path.clone(),
        };
        Ok(CentralServerConfig {
            url,
            ca_certificate_path,
            oidc,
        })
    }
}

fn required_value(variable: &'static str) -> Result<String, RoutingConfigError> {
    match env::var(variable) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => {
            Err(RoutingConfigError::MissingRequired { variable })
        }
        Err(env::VarError::NotUnicode(_)) => Err(RoutingConfigError::InvalidUnicode { variable }),
    }
}

fn required_url(variable: &'static str) -> Result<Url, RoutingConfigError> {
    let value = required_value(variable)?;
    Url::parse(&value).map_err(|source| RoutingConfigError::InvalidUrl {
        variable,
        value,
        source,
    })
}

#[derive(Debug, Error)]
pub enum RoutingConfigError {
    #[error("{variable} must be internal or centralized, got {value:?}")]
    InvalidPlacement {
        variable: &'static str,
        value: String,
    },
    #[error("{variable} is required when a resource route is centralized")]
    MissingRequired { variable: &'static str },
    #[error("{variable} contains non-Unicode data")]
    InvalidUnicode { variable: &'static str },
    #[error("{variable} must be a valid URL ({value:?}): {source}")]
    InvalidUrl {
        variable: &'static str,
        value: String,
        source: url::ParseError,
    },
    #[error("{variable} must use HTTPS, got {value}")]
    HttpsRequired {
        variable: &'static str,
        value: String,
    },
}
