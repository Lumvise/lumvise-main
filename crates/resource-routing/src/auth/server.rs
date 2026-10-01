use std::{collections::BTreeSet, path::Path, str::FromStr};

use crate::control::InvocationControl;
use jsonwebtoken::{
    Algorithm, DecodingKey, Header, Validation, decode, decode_header, jwk::JwkSet,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use url::Url;

/// Discovery/JWKS seam. Production refreshes a discovered key set once when
/// the token's `kid` is absent; fixtures inject an in-memory resolver.
pub trait JwksResolver: Send + Sync {
    fn key_for(
        &self,
        kid: &str,
        refresh: bool,
        control: &InvocationControl,
    ) -> Result<DecodingKey, OidcValidationError>;
}

/// Production discovery/JWKS implementation. It verifies HTTPS via reqwest's
/// Rustls trust store, validates discovery issuer identity, only accepts
/// advertised signing algorithms, and refreshes the JWKS exactly once for an
/// unknown `kid`.
pub struct OidcJwksResolver {
    issuer: String,
    jwks_uri: Url,
    algorithms: Vec<Algorithm>,
    client: reqwest::blocking::Client,
    keys: Mutex<Option<JwkSet>>,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: Url,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
}

impl OidcJwksResolver {
    pub fn discover(
        issuer: &str,
        control: &InvocationControl,
    ) -> Result<Self, OidcValidationError> {
        Self::discover_with_ca_certificate_path(issuer, None, control)
    }

    /// Discovers an HTTPS issuer using the platform trust store plus an
    /// optional PEM trust anchor for private OIDC PKI.
    ///
    /// Certificate and hostname verification remain enabled. The PEM only
    /// extends the root set used for discovery and subsequent JWKS refreshes.
    pub fn discover_with_ca_certificate_path(
        issuer: &str,
        ca_certificate_path: Option<&Path>,
        control: &InvocationControl,
    ) -> Result<Self, OidcValidationError> {
        let issuer = issuer.trim_end_matches('/');
        let issuer_url = Url::parse(issuer)
            .map_err(|error| OidcValidationError::Discovery(error.to_string()))?;
        if issuer_url.scheme() != "https" {
            return Err(OidcValidationError::Discovery(
                "OIDC issuer must use HTTPS".into(),
            ));
        }
        let client = oidc_https_client(ca_certificate_path)?;
        let document: DiscoveryDocument = fetch_json(
            &client,
            &format!("{}/.well-known/openid-configuration", issuer),
            control,
        )?;
        if document.issuer != issuer {
            return Err(OidcValidationError::Discovery(
                "discovery issuer does not match configured issuer".into(),
            ));
        }
        let algorithms = document
            .id_token_signing_alg_values_supported
            .iter()
            .filter_map(|value| Algorithm::from_str(value).ok())
            .collect::<Vec<_>>();
        if algorithms.is_empty() {
            return Err(OidcValidationError::NoAdvertisedAlgorithms);
        }
        Ok(Self {
            issuer: issuer.into(),
            jwks_uri: document.jwks_uri,
            algorithms,
            client,
            keys: Mutex::new(None),
        })
    }

    pub fn advertised_algorithms(&self) -> &[Algorithm] {
        &self.algorithms
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    fn refresh(&self, control: &InvocationControl) -> Result<(), OidcValidationError> {
        let keys = fetch_json(&self.client, self.jwks_uri.as_str(), control)?;
        *self.keys.lock() = Some(keys);
        Ok(())
    }
}

impl JwksResolver for OidcJwksResolver {
    fn key_for(
        &self,
        kid: &str,
        refresh: bool,
        control: &InvocationControl,
    ) -> Result<DecodingKey, OidcValidationError> {
        if refresh || self.keys.lock().is_none() {
            self.refresh(control)?;
        }
        let keys = self.keys.lock();
        let jwk = keys
            .as_ref()
            .and_then(|set| set.find(kid))
            .ok_or_else(|| OidcValidationError::UnknownKeyId(kid.into()))?;
        DecodingKey::from_jwk(jwk).map_err(OidcValidationError::Token)
    }
}

fn oidc_https_client(
    ca_certificate_path: Option<&Path>,
) -> Result<reqwest::blocking::Client, OidcValidationError> {
    let mut builder =
        reqwest::blocking::ClientBuilder::new().redirect(reqwest::redirect::Policy::none());
    if let Some(path) = ca_certificate_path {
        let pem = std::fs::read(path).map_err(|error| {
            OidcValidationError::Discovery(format!(
                "failed to read OIDC CA certificate {}: {error}",
                path.display()
            ))
        })?;
        let certificate = reqwest::Certificate::from_pem(&pem).map_err(|error| {
            OidcValidationError::Discovery(format!(
                "failed to parse OIDC CA certificate {}: {error}",
                path.display()
            ))
        })?;
        builder = builder.add_root_certificate(certificate);
    }
    builder
        .build()
        .map_err(|error| OidcValidationError::Discovery(error.to_string()))
}

fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::blocking::Client,
    url: &str,
    control: &InvocationControl,
) -> Result<T, OidcValidationError> {
    if control.is_cancelled() {
        return Err(OidcValidationError::Cancelled);
    }
    if control.is_expired() {
        return Err(OidcValidationError::DeadlineExceeded);
    }
    client
        .get(url)
        .timeout(control.remaining())
        .send()
        .map_err(|error| OidcValidationError::Discovery(error.to_string()))?
        .error_for_status()
        .map_err(|error| OidcValidationError::Discovery(error.to_string()))?
        .json()
        .map_err(|error| OidcValidationError::Discovery(error.to_string()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedPrincipal {
    pub issuer: String,
    pub subject: String,
    pub tenant_id: String,
    pub scopes: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct JwtValidationConfig {
    pub issuer: String,
    pub audience: String,
    pub tenant_claim: String,
    pub required_scope: String,
    /// Algorithms learned from issuer discovery/JWKS metadata. Callers must
    /// never provide `Algorithm::None` (jsonwebtoken does not expose it).
    pub advertised_algorithms: Vec<Algorithm>,
}

pub struct JwtValidator {
    config: JwtValidationConfig,
}

impl JwtValidator {
    pub fn new(config: JwtValidationConfig) -> Result<Self, OidcValidationError> {
        if config.advertised_algorithms.is_empty() {
            return Err(OidcValidationError::NoAdvertisedAlgorithms);
        }
        Ok(Self { config })
    }

    pub fn validate_with_jwks(
        &self,
        token: &str,
        resolver: &dyn JwksResolver,
        control: &InvocationControl,
    ) -> Result<AuthenticatedPrincipal, OidcValidationError> {
        let header =
            decode_header(token).map_err(|_| OidcValidationError::OpaqueOrMalformedAccessToken)?;
        if !self.config.advertised_algorithms.contains(&header.alg) {
            return Err(OidcValidationError::DisallowedAlgorithm(header.alg));
        }
        let kid = header.kid.ok_or(OidcValidationError::MissingKeyId)?;
        let key = match resolver.key_for(&kid, false, control) {
            Ok(key) => key,
            Err(OidcValidationError::UnknownKeyId(_)) => resolver.key_for(&kid, true, control)?,
            Err(error) => return Err(error),
        };
        self.validate_with_key(token, &key)
    }

    /// Verifies a signed JWT after the caller selected its `kid` from a
    /// discovered JWKS. Header algorithm restrictions are applied before
    /// claims are inspected. An opaque token never reaches claim parsing.
    pub fn validate_with_key(
        &self,
        token: &str,
        key: &DecodingKey,
    ) -> Result<AuthenticatedPrincipal, OidcValidationError> {
        let header =
            decode_header(token).map_err(|_| OidcValidationError::OpaqueOrMalformedAccessToken)?;
        if !self.config.advertised_algorithms.contains(&header.alg) {
            return Err(OidcValidationError::DisallowedAlgorithm(header.alg));
        }
        reject_id_token_header(&header)?;
        let mut validation = Validation::new(header.alg);
        validation.algorithms = self.config.advertised_algorithms.clone();
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.set_required_spec_claims(&["iss", "aud", "exp", "sub"]);
        let data = decode::<Claims>(token, key, &validation).map_err(OidcValidationError::Token)?;
        if data.claims.nonce.is_some() || data.claims.token_use.as_deref() == Some("id") {
            return Err(OidcValidationError::IdTokenNotAccepted);
        }
        let tenant_id = data
            .claims
            .extra
            .get(&self.config.tenant_claim)
            .and_then(Value::as_str)
            .filter(|tenant| !tenant.is_empty())
            .ok_or_else(|| {
                OidcValidationError::MissingTenantClaim(self.config.tenant_claim.clone())
            })?
            .to_owned();
        let scopes = data.claims.scopes();
        if !scopes.contains(&self.config.required_scope) {
            return Err(OidcValidationError::MissingScope(
                self.config.required_scope.clone(),
            ));
        }
        Ok(AuthenticatedPrincipal {
            issuer: data.claims.iss,
            subject: data.claims.sub,
            tenant_id,
            scopes,
        })
    }
}

fn reject_id_token_header(header: &Header) -> Result<(), OidcValidationError> {
    if header
        .typ
        .as_deref()
        .is_some_and(|typ| typ.eq_ignore_ascii_case("id+jwt"))
    {
        return Err(OidcValidationError::IdTokenNotAccepted);
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    #[serde(default)]
    scope: Option<Value>,
    #[serde(default)]
    scp: Option<Vec<String>>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    token_use: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

impl Claims {
    fn scopes(&self) -> BTreeSet<String> {
        let mut scopes = BTreeSet::new();
        match self.scope.as_ref() {
            Some(Value::String(value)) => {
                scopes.extend(value.split_ascii_whitespace().map(str::to_owned))
            }
            Some(Value::Array(values)) => {
                scopes.extend(values.iter().filter_map(Value::as_str).map(str::to_owned))
            }
            _ => {}
        }
        scopes.extend(self.scp.iter().flatten().cloned());
        scopes
    }
}

#[derive(Debug, Error)]
pub enum OidcValidationError {
    #[error("OIDC issuer did not advertise an accepted JWT signing algorithm")]
    NoAdvertisedAlgorithms,
    #[error("OIDC discovery or JWKS retrieval failed: {0}")]
    Discovery(String),
    #[error("shared invocation was cancelled")]
    Cancelled,
    #[error("shared invocation deadline expired")]
    DeadlineExceeded,
    #[error("access token is opaque or malformed; signed JWT access tokens are required")]
    OpaqueOrMalformedAccessToken,
    #[error("JWT algorithm {0:?} was not advertised by the issuer")]
    DisallowedAlgorithm(Algorithm),
    #[error("signed JWT has no key ID")]
    MissingKeyId,
    #[error("issuer JWKS has no signing key {0:?}")]
    UnknownKeyId(String),
    #[error("ID tokens cannot be used as resource API credentials")]
    IdTokenNotAccepted,
    #[error("signed JWT validation failed: {0}")]
    Token(jsonwebtoken::errors::Error),
    #[error("signed JWT does not have string tenant claim {0:?}")]
    MissingTenantClaim(String),
    #[error("signed JWT does not contain required scope {0:?}")]
    MissingScope(String),
}
