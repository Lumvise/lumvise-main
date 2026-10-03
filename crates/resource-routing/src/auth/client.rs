use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use openidconnect::{
    AuthorizationCode, ClientId, CsrfToken, IssuerUrl, Nonce, OAuth2TokenResponse,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, Scope,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{config::OidcClientConfig, control::InvocationControl};

/// Persistence boundary deliberately limited to refresh tokens. Production
/// composition uses [`KeyringCredentialStore`]; tests inject a memory store.
pub trait CredentialStore: Send + Sync {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, OidcClientError>;
    fn store(&self, service: &str, account: &str, secret: &str) -> Result<(), OidcClientError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), OidcClientError>;
}

pub trait BrowserLauncher: Send + Sync {
    fn open(&self, authorization_url: &str) -> Result<(), OidcClientError>;
}

pub trait CallbackReceiver: Send + Sync {
    fn receive(&self, control: &InvocationControl) -> Result<OidcCallback, OidcClientError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcCallback {
    pub state: String,
    pub code: String,
}

/// Production ephemeral native-app loopback callback receiver. The listener
/// binds only 127.0.0.1 and is consumed after one callback.
pub struct LoopbackCallbackReceiver {
    listener: TcpListener,
}

impl LoopbackCallbackReceiver {
    pub fn bind() -> Result<(Self, String), OidcClientError> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(OidcClientError::CallbackIo)?;
        let address = listener.local_addr().map_err(OidcClientError::CallbackIo)?;
        listener
            .set_nonblocking(true)
            .map_err(OidcClientError::CallbackIo)?;
        Ok((Self { listener }, format!("http://{address}/callback")))
    }
}

impl CallbackReceiver for LoopbackCallbackReceiver {
    fn receive(&self, control: &InvocationControl) -> Result<OidcCallback, OidcClientError> {
        loop {
            if control.is_cancelled() {
                return Err(OidcClientError::Cancelled);
            }
            if control.is_expired() {
                return Err(OidcClientError::DeadlineExceeded);
            }
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = String::new();
                    BufReader::new(&stream)
                        .read_line(&mut request)
                        .map_err(OidcClientError::CallbackIo)?;
                    let target = request
                        .split_ascii_whitespace()
                        .nth(1)
                        .ok_or(OidcClientError::MalformedCallback)?;
                    let callback_url = url::Url::parse(&format!("http://127.0.0.1{target}"))
                        .map_err(|_| OidcClientError::MalformedCallback)?;
                    let callback = OidcCallback {
                        state: callback_url
                            .query_pairs()
                            .find(|(name, _)| name == "state")
                            .map(|(_, value)| value.into_owned())
                            .ok_or(OidcClientError::MalformedCallback)?,
                        code: callback_url
                            .query_pairs()
                            .find(|(name, _)| name == "code")
                            .map(|(_, value)| value.into_owned())
                            .ok_or(OidcClientError::MissingAuthorizationCode)?,
                    };
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 28\r\n\r\nAuthentication completed. Return.")
                        .map_err(OidcClientError::CallbackIo)?;
                    return Ok(callback);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(OidcClientError::CallbackIo(error)),
            }
        }
    }
}

pub struct SystemBrowserLauncher;

impl BrowserLauncher for SystemBrowserLauncher {
    fn open(&self, authorization_url: &str) -> Result<(), OidcClientError> {
        #[cfg(target_os = "macos")]
        let command = "open";
        #[cfg(target_os = "windows")]
        let command = "cmd";
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        let command = "xdg-open";
        #[cfg(target_os = "windows")]
        let status = std::process::Command::new(command)
            .args(["/C", "start", authorization_url])
            .status();
        #[cfg(not(target_os = "windows"))]
        let status = std::process::Command::new(command)
            .arg(authorization_url)
            .status();
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(OidcClientError::BrowserLaunch(format!(
                "browser exited {status}"
            ))),
            Err(error) => Err(OidcClientError::BrowserLaunch(error.to_string())),
        }
    }
}

/// Drives the HTTPS authorization redirect emitted by the assistant end-to-end
/// OIDC fixture. This launcher is deliberately unavailable to production
/// builds: production continues to hand authorization to the system browser.
///
/// The fixture CA is installed as an explicit trust anchor for the initial
/// authorization request. The redirect is then issued asynchronously so the
/// existing loopback receiver can accept it and preserve the normal
/// state/nonce/PKCE validation path in [`OidcClient::authenticate`].
#[cfg(feature = "assistant-e2e")]
pub struct FixtureBrowserLauncher {
    ca_certificate_path: std::path::PathBuf,
}

#[cfg(feature = "assistant-e2e")]
impl FixtureBrowserLauncher {
    pub fn new(ca_certificate_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            ca_certificate_path: ca_certificate_path.into(),
        }
    }
}

#[cfg(feature = "assistant-e2e")]
impl BrowserLauncher for FixtureBrowserLauncher {
    fn open(&self, authorization_url: &str) -> Result<(), OidcClientError> {
        let authorization_url = url::Url::parse(authorization_url)
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        if authorization_url.scheme() != "https" {
            return Err(OidcClientError::BrowserLaunch(
                "assistant-e2e authorization URL must use HTTPS".into(),
            ));
        }
        let certificate = std::fs::read(&self.ca_certificate_path)
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        let certificate = reqwest::Certificate::from_pem(&certificate)
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(certificate)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        let response = client
            .get(authorization_url)
            .send()
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        if !response.status().is_redirection() {
            return Err(OidcClientError::BrowserLaunch(format!(
                "fixture authorization endpoint returned {} instead of a redirect",
                response.status()
            )));
        }
        let redirect = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| {
                OidcClientError::BrowserLaunch("fixture redirect omitted Location".into())
            })?
            .to_str()
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        let redirect = response
            .url()
            .join(redirect)
            .map_err(|error| OidcClientError::BrowserLaunch(error.to_string()))?;
        if redirect.scheme() != "http" || redirect.host_str() != Some("127.0.0.1") {
            return Err(OidcClientError::BrowserLaunch(
                "fixture authorization redirect must target the loopback callback".into(),
            ));
        }
        std::thread::spawn(move || {
            let _ = reqwest::blocking::get(redirect);
        });
        Ok(())
    }
}

/// OS credential-store implementation. This is the only production durable
/// secret path and stores no access or ID token.
///
/// Persistent only on macOS (login Keychain via keyring's `apple-native`).
/// Other platforms fall back to keyring's in-memory mock, so saved sign-ins
/// are lost on quit there until a platform backend is enabled.
pub struct KeyringCredentialStore;

impl CredentialStore for KeyringCredentialStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, OidcClientError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| OidcClientError::CredentialStore(error.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(OidcClientError::CredentialStore(error.to_string())),
        }
    }
    fn store(&self, service: &str, account: &str, secret: &str) -> Result<(), OidcClientError> {
        keyring::Entry::new(service, account)
            .map_err(|error| OidcClientError::CredentialStore(error.to_string()))?
            .set_password(secret)
            .map_err(|error| OidcClientError::CredentialStore(error.to_string()))
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), OidcClientError> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| OidcClientError::CredentialStore(error.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(OidcClientError::CredentialStore(error.to_string())),
        }
    }
}

/// Deterministic test credential store; never use for desktop composition.
#[derive(Default)]
pub struct RefreshTokenStore {
    values: Mutex<HashMap<String, String>>,
}

impl CredentialStore for RefreshTokenStore {
    fn load(&self, _: &str, account: &str) -> Result<Option<String>, OidcClientError> {
        Ok(self.values.lock().get(account).cloned())
    }
    fn store(&self, _: &str, account: &str, secret: &str) -> Result<(), OidcClientError> {
        self.values.lock().insert(account.into(), secret.into());
        Ok(())
    }
    fn delete(&self, _: &str, account: &str) -> Result<(), OidcClientError> {
        self.values.lock().remove(account);
        Ok(())
    }
}

/// Auth Code + PKCE values held only for the active loopback interaction.
pub struct OidcLoginAttempt {
    pub authorization_url: String,
    csrf_state: CsrfToken,
    nonce: Nonce,
    pkce_verifier: PkceCodeVerifier,
}

impl OidcLoginAttempt {
    pub fn new(
        authorization_endpoint: &str,
        client_id: &str,
        redirect_uri: &str,
        scopes: &[String],
    ) -> Result<Self, OidcClientError> {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let csrf_state = CsrfToken::new_random();
        let nonce = Nonce::new_random();
        let mut url = url::Url::parse(authorization_endpoint)
            .map_err(|error| OidcClientError::AuthorizationUrl(error.to_string()))?;
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("scope", &scopes.join(" "));
        query.append_pair("state", csrf_state.secret());
        query.append_pair("nonce", nonce.secret());
        query.append_pair("code_challenge", challenge.as_str());
        query.append_pair("code_challenge_method", "S256");
        drop(query);
        Ok(Self {
            authorization_url: url.into(),
            csrf_state,
            nonce,
            pkce_verifier: verifier,
        })
    }

    pub fn validate_callback(&self, callback: &OidcCallback) -> Result<(), OidcClientError> {
        if callback.state != *self.csrf_state.secret() {
            return Err(OidcClientError::StateMismatch);
        }
        if callback.code.is_empty() {
            return Err(OidcClientError::MissingAuthorizationCode);
        }
        Ok(())
    }

    pub fn pkce_verifier(&self) -> &PkceCodeVerifier {
        &self.pkce_verifier
    }
    pub fn nonce(&self) -> &Nonce {
        &self.nonce
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OidcAccessToken {
    pub value: String,
    pub expires_at_unix_ms: u64,
}

/// One configured issuer/client identity. `complete_authorization` is a
/// composition seam: browser, callback, credential storage, and the shared
/// invocation deadline are all injected rather than hidden global state.
pub struct OidcClient {
    config: OidcClientConfig,
    credentials: Arc<dyn CredentialStore>,
    refresh_lock: Mutex<()>,
}

impl OidcClient {
    pub const CREDENTIAL_SERVICE: &'static str = "lumvise.central-resources";

    pub fn new(config: OidcClientConfig, credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            config,
            credentials,
            refresh_lock: Mutex::new(()),
        }
    }

    pub fn account_name(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.config.issuer_url.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(self.config.client_id.as_bytes());
        format!("active-{:x}", hasher.finalize())
    }

    pub fn stored_refresh_token(&self) -> Result<Option<String>, OidcClientError> {
        self.credentials
            .load(Self::CREDENTIAL_SERVICE, &self.account_name())
    }
    pub fn save_refresh_token(&self, refresh_token: &str) -> Result<(), OidcClientError> {
        self.credentials.store(
            Self::CREDENTIAL_SERVICE,
            &self.account_name(),
            refresh_token,
        )
    }
    pub fn clear_refresh_token(&self) -> Result<(), OidcClientError> {
        self.credentials
            .delete(Self::CREDENTIAL_SERVICE, &self.account_name())
    }

    pub fn begin_authorization(
        &self,
        authorization_endpoint: &str,
        redirect_uri: &str,
    ) -> Result<OidcLoginAttempt, OidcClientError> {
        OidcLoginAttempt::new(
            authorization_endpoint,
            &self.config.client_id,
            redirect_uri,
            &self.config.scopes,
        )
    }

    pub fn launch_and_receive(
        &self,
        launcher: &dyn BrowserLauncher,
        receiver: &dyn CallbackReceiver,
        attempt: &OidcLoginAttempt,
        control: &InvocationControl,
    ) -> Result<OidcCallback, OidcClientError> {
        if control.is_cancelled() {
            return Err(OidcClientError::Cancelled);
        }
        if control.is_expired() {
            return Err(OidcClientError::DeadlineExceeded);
        }
        launcher.open(&attempt.authorization_url)?;
        let callback = receiver.receive(control)?;
        attempt.validate_callback(&callback)?;
        Ok(callback)
    }

    /// Real native-app Authorization Code + PKCE flow: discovery, ephemeral
    /// loopback redirect, state/nonce verification, code exchange, then
    /// refresh-token persistence. It is intentionally synchronous because the
    /// desktop composition root is synchronous; every blocking phase observes
    /// the one parent invocation control.
    pub fn authenticate(
        &self,
        launcher: &dyn BrowserLauncher,
        control: &InvocationControl,
    ) -> Result<OidcAccessToken, OidcClientError> {
        let (receiver, redirect_uri) = LoopbackCallbackReceiver::bind()?;
        let http = oidc_http_client(
            self.config.ca_certificate_path.as_deref(),
            control.remaining(),
        )?;
        let issuer = IssuerUrl::new(
            self.config
                .issuer_url
                .to_string()
                .trim_end_matches('/')
                .to_owned(),
        )
        .map_err(|error| OidcClientError::Discovery(error.to_string()))?;
        let metadata = CoreProviderMetadata::discover(&issuer, &http)
            .map_err(|error| OidcClientError::Discovery(error.to_string()))?;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.client_id.clone()),
            None,
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri.clone())
                .map_err(|error| OidcClientError::Discovery(error.to_string()))?,
        );
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (authorization_url, state, nonce) = self.authorize(&client, challenge);
        launcher.open(authorization_url.as_str())?;
        let callback = receiver.receive(control)?;
        if callback.state != *state.secret() {
            return Err(OidcClientError::StateMismatch);
        }
        let response = client
            .exchange_code(AuthorizationCode::new(callback.code))
            .map_err(|error| OidcClientError::TokenExchange(error.to_string()))?
            .set_pkce_verifier(verifier)
            .request(&http)
            .map_err(|error| OidcClientError::TokenExchange(error.to_string()))?;
        let id_token = response
            .extra_fields()
            .id_token()
            .ok_or(OidcClientError::MissingIdToken)?;
        id_token
            .claims(&client.id_token_verifier(), &nonce)
            .map_err(|error| OidcClientError::IdTokenVerification(error.to_string()))?;
        if let Some(refresh_token) = response.refresh_token() {
            self.save_refresh_token(refresh_token.secret())?;
        }
        Ok(OidcAccessToken {
            value: response.access_token().secret().to_owned(),
            expires_at_unix_ms: expires_at_unix_ms(response.expires_in()),
        })
    }

    /// Refreshes exactly once under the shared lock when the cached access
    /// token cannot survive the parent deadline. Providers that do not return
    /// a refresh token fail closed rather than prompting or falling back.
    pub fn refresh_access_token(
        &self,
        control: &InvocationControl,
    ) -> Result<OidcAccessToken, OidcClientError> {
        self.with_refresh_lock(control, || {
            let refresh_token = self
                .stored_refresh_token()?
                .ok_or(OidcClientError::MissingRefreshToken)?;
            let issuer = IssuerUrl::new(
                self.config
                    .issuer_url
                    .to_string()
                    .trim_end_matches('/')
                    .to_owned(),
            )
            .map_err(|error| OidcClientError::Discovery(error.to_string()))?;
            let http = oidc_http_client(
                self.config.ca_certificate_path.as_deref(),
                control.remaining(),
            )?;
            let metadata = CoreProviderMetadata::discover(&issuer, &http)
                .map_err(|error| OidcClientError::Discovery(error.to_string()))?;
            let client = CoreClient::from_provider_metadata(
                metadata,
                ClientId::new(self.config.client_id.clone()),
                None,
            );
            let response = client
                .exchange_refresh_token(&RefreshToken::new(refresh_token))
                .map_err(|error| OidcClientError::TokenExchange(error.to_string()))?
                .request(&http)
                .map_err(|error| OidcClientError::TokenExchange(error.to_string()))?;
            if let Some(rotated) = response.refresh_token() {
                self.save_refresh_token(rotated.secret())?;
            }
            Ok(OidcAccessToken {
                value: response.access_token().secret().to_owned(),
                expires_at_unix_ms: expires_at_unix_ms(response.expires_in()),
            })
        })
    }

    /// Returns the in-memory access token only when it remains valid through
    /// the parent invocation deadline; otherwise performs one serialized
    /// refresh. A missing access token begins the native PKCE flow.
    pub fn access_token_for_invocation(
        &self,
        current: Option<OidcAccessToken>,
        launcher: &dyn BrowserLauncher,
        control: &InvocationControl,
    ) -> Result<OidcAccessToken, OidcClientError> {
        match current {
            Some(token) if token.expires_at_unix_ms > control.deadline_unix_ms() => Ok(token),
            Some(_) => self.refresh_access_token(control),
            None => self.authenticate(launcher, control),
        }
    }

    fn authorize(
        &self,
        client: &CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        challenge: PkceCodeChallenge,
    ) -> (url::Url, CsrfToken, Nonce) {
        let mut request = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        for scope in &self.config.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        request.url()
    }

    pub fn with_refresh_lock<T>(
        &self,
        control: &InvocationControl,
        operation: impl FnOnce() -> Result<T, OidcClientError>,
    ) -> Result<T, OidcClientError> {
        if control.is_cancelled() {
            return Err(OidcClientError::Cancelled);
        }
        if control.is_expired() {
            return Err(OidcClientError::DeadlineExceeded);
        }
        let _guard = self.refresh_lock.lock();
        if control.is_cancelled() {
            return Err(OidcClientError::Cancelled);
        }
        if control.is_expired() {
            return Err(OidcClientError::DeadlineExceeded);
        }
        operation()
    }
}

fn expires_at_unix_ms(expires_in: Option<Duration>) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock predates Unix epoch")
        .saturating_add(expires_in.unwrap_or(Duration::ZERO))
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn oidc_http_client(
    ca_certificate_path: Option<&Path>,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, OidcClientError> {
    let mut builder =
        reqwest::blocking::ClientBuilder::new().redirect(reqwest::redirect::Policy::none());
    if let Some(path) = ca_certificate_path {
        let pem = std::fs::read(path).map_err(|error| {
            OidcClientError::Discovery(format!(
                "failed to read OIDC CA certificate {}: {error}",
                path.display()
            ))
        })?;
        let cert = reqwest::Certificate::from_pem(&pem).map_err(|error| {
            OidcClientError::Discovery(format!(
                "failed to parse OIDC CA certificate {}: {error}",
                path.display()
            ))
        })?;
        builder = builder.add_root_certificate(cert);
    }
    builder
        .timeout(timeout)
        .build()
        .map_err(|error| OidcClientError::Http(error.to_string()))
}

#[derive(Debug, Error)]
pub enum OidcClientError {
    #[error("OIDC authorization URL is invalid: {0}")]
    AuthorizationUrl(String),
    #[error("OIDC callback state does not match the generated PKCE state")]
    StateMismatch,
    #[error("OIDC callback did not include an authorization code")]
    MissingAuthorizationCode,
    #[error("OIDC callback is malformed")]
    MalformedCallback,
    #[error("loopback callback I/O failed: {0}")]
    CallbackIo(#[source] std::io::Error),
    #[error("could not open system browser: {0}")]
    BrowserLaunch(String),
    #[error("OIDC discovery failed: {0}")]
    Discovery(String),
    #[error("OIDC token exchange failed: {0}")]
    TokenExchange(String),
    #[error("OIDC token response omitted the required ID token")]
    MissingIdToken,
    #[error("OIDC ID token nonce/signature verification failed: {0}")]
    IdTokenVerification(String),
    #[error("no persisted OIDC refresh token is available")]
    MissingRefreshToken,
    #[error("OIDC HTTP client failed: {0}")]
    Http(String),
    #[error("the shared invocation was cancelled")]
    Cancelled,
    #[error("the shared invocation deadline expired")]
    DeadlineExceeded,
    #[error("credential store error: {0}")]
    CredentialStore(String),
}

#[cfg(test)]
mod tests {
    use super::{CredentialStore, KeyringCredentialStore};

    fn uses_mock_backend() -> bool {
        keyring::Entry::new("com.lumvise.backend-probe", "probe")
            .unwrap()
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .is_some()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keyring_store_uses_the_login_keychain() {
        assert!(
            !uses_mock_backend(),
            "keyring fell back to its in-memory mock; expected apple-native"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_keyring_store_is_documented_as_in_memory() {
        assert!(
            uses_mock_backend(),
            "a persistent backend is now enabled; update the README and KeyringCredentialStore docs"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keyring_store_round_trips_through_a_fresh_entry() {
        let service = format!("com.lumvise.test.{}", std::process::id());
        let store = KeyringCredentialStore;
        store.store(&service, "account", "refresh-secret").unwrap();
        let loaded = store.load(&service, "account");
        store.delete(&service, "account").unwrap();
        assert_eq!(loaded.unwrap().as_deref(), Some("refresh-secret"));
        assert_eq!(store.load(&service, "account").unwrap(), None);
    }
}
