//! Local OIDC fixture for supervised resource-server and desktop smoke checks.
//!
//! This executable is deliberately an example target: it generates a short-lived
//! local CA plus a certificate usable by both this issuer and the resource
//! server, then serves an Authorization Code + PKCE issuer over HTTPS.

use std::{
    collections::HashMap,
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    PKCS_ECDSA_P256_SHA256,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, oneshot},
};
use tokio_rustls::{TlsAcceptor, rustls};
use url::Url;
use uuid::Uuid;

const ACCESS_TOKEN_AUDIENCE: &str = "lumvise-resources";
const KEY_ID: &str = "fixture-signing-key";
const ACCESS_SCOPE: &str = "lumvise.resources lumvise.resources.migrate";
const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
struct FixtureInfo {
    issuer_url: String,
    ca_certificate_path: PathBuf,
    server_certificate_path: PathBuf,
    server_key_path: PathBuf,
}

impl FixtureInfo {
    fn print_environment(&self) {
        println!("OIDC fixture is listening at {}", self.issuer_url);
        println!(
            "OIDC_FIXTURE_CA_CERT_PATH={}",
            self.ca_certificate_path.display()
        );
        println!(
            "OIDC_FIXTURE_SERVER_CERT_PATH={}",
            self.server_certificate_path.display()
        );
        println!(
            "OIDC_FIXTURE_SERVER_KEY_PATH={}",
            self.server_key_path.display()
        );
        println!("SSL_CERT_FILE={}", self.ca_certificate_path.display());
        println!(
            "LUMVISE_OIDC_CA_CERT_PATH={}",
            self.ca_certificate_path.display()
        );
        println!("LUMVISE_OIDC_ISSUER_URL={}", self.issuer_url);
        println!(
            "LUMVISE_RESOURCE_SERVER_TLS_CERT_PATH={}",
            self.server_certificate_path.display()
        );
        println!(
            "LUMVISE_RESOURCE_SERVER_TLS_KEY_PATH={}",
            self.server_key_path.display()
        );
        println!("LUMVISE_OIDC_AUDIENCE={ACCESS_TOKEN_AUDIENCE}");
    }
}

struct Fixture {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    state: Arc<FixtureState>,
    info: FixtureInfo,
}

struct FixtureState {
    issuer_url: String,
    signing_key: EncodingKey,
    jwks: Value,
    authorization_codes: Mutex<HashMap<String, AuthorizationCodeRecord>>,
}

struct AuthorizationCodeRecord {
    code_challenge: String,
    redirect_uri: String,
    nonce: String,
}

#[derive(Serialize)]
struct AccessTokenClaims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    exp: u64,
    iat: u64,
    scope: &'a str,
    tenant_id: &'a str,
    token_use: &'a str,
}

#[derive(Serialize)]
struct IdTokenClaims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    exp: u64,
    iat: u64,
    nonce: &'a str,
}

impl Fixture {
    async fn bind(
        port: u16,
        artifact_directory: &Path,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        std::fs::create_dir_all(artifact_directory)?;
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let bound = listener.local_addr()?;
        let issuer_url = format!("https://127.0.0.1:{}", bound.port());
        let generated = GeneratedMaterial::create(artifact_directory)?;

        let info = FixtureInfo {
            issuer_url: issuer_url.clone(),
            ca_certificate_path: generated.ca_certificate_path,
            server_certificate_path: generated.server_certificate_path,
            server_key_path: generated.server_key_path,
        };
        let state = Arc::new(FixtureState {
            issuer_url,
            signing_key: generated.signing_key,
            jwks: generated.jwks,
            authorization_codes: Mutex::new(HashMap::new()),
        });
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![generated.server_certificate_der],
                generated.server_private_key,
            )?;

        Ok(Self {
            listener,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
            state,
            info,
        })
    }

    async fn run(
        self,
        mut shutdown: oneshot::Receiver<()>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let acceptor = self.acceptor.clone();
                    let state = Arc::clone(&self.state);
                    tokio::spawn(async move {
                        if let Err(error) = serve_connection(acceptor, stream, state).await {
                            eprintln!("OIDC fixture connection failed: {error}");
                        }
                    });
                }
            }
        }
    }
}

struct GeneratedMaterial {
    ca_certificate_path: PathBuf,
    server_certificate_path: PathBuf,
    server_key_path: PathBuf,
    server_certificate_der: rustls::pki_types::CertificateDer<'static>,
    server_private_key: rustls::pki_types::PrivateKeyDer<'static>,
    signing_key: EncodingKey,
    jwks: Value,
}

impl GeneratedMaterial {
    fn create(artifact_directory: &Path) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "Lumvise OIDC fixture CA");
        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let ca_certificate = ca_params.self_signed(&ca_key)?;
        let issuer = Issuer::new(ca_params, ca_key);

        let mut server_params =
            CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])?;
        server_params
            .distinguished_name
            .push(DnType::CommonName, "Lumvise OIDC fixture server");
        let server_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let server_certificate = server_params.signed_by(&server_key, &issuer)?;

        let signing_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let encoding_key = EncodingKey::from_ec_der(&signing_key.serialize_der());
        let mut jwk =
            serde_json::to_value(Jwk::from_encoding_key(&encoding_key, Algorithm::ES256)?)?;
        let jwk_object = jwk
            .as_object_mut()
            .ok_or("jsonwebtoken produced a non-object JWK")?;
        jwk_object.insert("kid".into(), Value::String(KEY_ID.into()));
        jwk_object.insert("use".into(), Value::String("sig".into()));
        jwk_object.insert("alg".into(), Value::String("ES256".into()));

        let ca_certificate_path = artifact_directory.join("oidc-fixture-ca.pem");
        let server_certificate_path = artifact_directory.join("oidc-fixture-server.pem");
        let server_key_path = artifact_directory.join("oidc-fixture-server-key.pem");
        std::fs::write(&ca_certificate_path, ca_certificate.pem())?;
        std::fs::write(&server_certificate_path, server_certificate.pem())?;
        std::fs::write(&server_key_path, server_key.serialize_pem())?;

        Ok(Self {
            ca_certificate_path,
            server_certificate_path,
            server_key_path,
            server_certificate_der: server_certificate.der().clone(),
            server_private_key: rustls::pki_types::PrivateKeyDer::Pkcs8(
                rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()),
            ),
            signing_key: encoding_key,
            jwks: json!({"keys": [jwk]}),
        })
    }
}

async fn serve_connection(
    acceptor: TlsAcceptor,
    stream: TcpStream,
    state: Arc<FixtureState>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stream = acceptor.accept(stream).await?;
    let request = read_request(&mut stream).await?;
    let response = route_request(request, state).await;
    write_response(&mut stream, response).await?;
    Ok(())
}

struct HttpRequest {
    method: String,
    target: String,
    body: Vec<u8>,
}

async fn read_request<S>(stream: &mut S) -> Result<HttpRequest, Box<dyn Error + Send + Sync>>
where
    S: AsyncReadExt + Unpin,
{
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("request headers exceed fixture limit".into());
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err("connection closed before request headers".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
    };
    let header_text = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or("missing request line")?;
    let mut request_parts = request_line.split_ascii_whitespace();
    let method = request_parts
        .next()
        .ok_or("missing HTTP method")?
        .to_owned();
    let target = request_parts
        .next()
        .ok_or("missing request target")?
        .to_owned();
    if request_parts.next().is_none() {
        return Err("missing HTTP version".into());
    }
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if content_length > MAX_REQUEST_BYTES {
        return Err("request body exceeds fixture limit".into());
    }
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err("connection closed before request body".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(HttpRequest {
        method,
        target,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&value).expect("JSON value must serialize"),
        }
    }

    fn redirect(location: String) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".into(), location)],
            body: Vec::new(),
        }
    }

    fn bad_request(message: &str) -> Self {
        Self {
            status: 400,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(
                &json!({"error": "invalid_request", "error_description": message}),
            )
            .expect("JSON value must serialize"),
        }
    }

    fn not_found() -> Self {
        Self {
            status: 404,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&json!({"error": "not_found"}))
                .expect("JSON value must serialize"),
        }
    }
}

async fn route_request(request: HttpRequest, state: Arc<FixtureState>) -> HttpResponse {
    let url = match Url::parse(&format!("{}{}", state.issuer_url, request.target)) {
        Ok(url) => url,
        Err(_) => return HttpResponse::bad_request("invalid request target"),
    };
    match (request.method.as_str(), url.path()) {
        ("GET", "/.well-known/openid-configuration") => HttpResponse::json(json!({
            "issuer": state.issuer_url,
            "authorization_endpoint": format!("{}/authorize", state.issuer_url),
            "token_endpoint": format!("{}/token", state.issuer_url),
            "jwks_uri": format!("{}/jwks", state.issuer_url),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "id_token_signing_alg_values_supported": ["ES256"],
            "subject_types_supported": ["public"]
        })),
        ("GET", "/jwks") => HttpResponse::json(state.jwks.clone()),
        ("GET", "/authorize") => authorize(&url, state).await,
        ("POST", "/token") => exchange_token(&request.body, state).await,
        _ => HttpResponse::not_found(),
    }
}

async fn authorize(url: &Url, state: Arc<FixtureState>) -> HttpResponse {
    let query = query_map(url.query());
    if query.get("response_type") != Some(&"code".to_owned()) {
        return HttpResponse::bad_request("response_type must be code");
    }
    let Some(redirect_uri) = query.get("redirect_uri").filter(|value| !value.is_empty()) else {
        return HttpResponse::bad_request("redirect_uri is required");
    };
    let Some(state_value) = query.get("state").filter(|value| !value.is_empty()) else {
        return HttpResponse::bad_request("state is required");
    };
    let Some(code_challenge) = query
        .get("code_challenge")
        .filter(|value| !value.is_empty())
    else {
        return HttpResponse::bad_request("code_challenge is required");
    };
    if query.get("code_challenge_method") != Some(&"S256".to_owned()) {
        return HttpResponse::bad_request("code_challenge_method must be S256");
    }
    let nonce = query.get("nonce").cloned().unwrap_or_default();
    let code = Uuid::new_v4().to_string();
    state.authorization_codes.lock().await.insert(
        code.clone(),
        AuthorizationCodeRecord {
            code_challenge: code_challenge.clone(),
            redirect_uri: redirect_uri.clone(),
            nonce,
        },
    );
    let mut callback = match Url::parse(redirect_uri) {
        Ok(url) => url,
        Err(_) => return HttpResponse::bad_request("redirect_uri must be an absolute URL"),
    };
    callback
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", state_value);
    HttpResponse::redirect(callback.into())
}

async fn exchange_token(body: &[u8], state: Arc<FixtureState>) -> HttpResponse {
    let form = form_map(body);
    if form.get("grant_type") != Some(&"authorization_code".to_owned()) {
        return HttpResponse::bad_request("grant_type must be authorization_code");
    }
    let Some(code) = form.get("code") else {
        return HttpResponse::bad_request("code is required");
    };
    let Some(verifier) = form.get("code_verifier") else {
        return HttpResponse::bad_request("code_verifier is required");
    };
    let Some(redirect_uri) = form.get("redirect_uri") else {
        return HttpResponse::bad_request("redirect_uri is required");
    };
    let record = match state.authorization_codes.lock().await.remove(code) {
        Some(record) => record,
        None => return HttpResponse::bad_request("unknown or already used authorization code"),
    };
    if record.redirect_uri != *redirect_uri {
        return HttpResponse::bad_request("redirect_uri does not match authorization request");
    }
    if pkce_challenge(verifier) != record.code_challenge {
        return HttpResponse::bad_request("code_verifier does not match PKCE challenge");
    }
    let now = unix_time();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(KEY_ID.into());
    let access_token = match encode(
        &header,
        &AccessTokenClaims {
            iss: &state.issuer_url,
            sub: "fixture-user",
            aud: ACCESS_TOKEN_AUDIENCE,
            exp: now + Duration::from_secs(300).as_secs(),
            iat: now,
            scope: ACCESS_SCOPE,
            tenant_id: "tenant-a",
            token_use: "access",
        },
        &state.signing_key,
    ) {
        Ok(token) => token,
        Err(_) => return HttpResponse::bad_request("could not sign access token"),
    };
    let id_token = match encode(
        &header,
        &IdTokenClaims {
            iss: &state.issuer_url,
            sub: "fixture-user",
            aud: form.get("client_id").map_or("desktop", String::as_str),
            exp: now + Duration::from_secs(300).as_secs(),
            iat: now,
            nonce: &record.nonce,
        },
        &state.signing_key,
    ) {
        Ok(token) => token,
        Err(_) => return HttpResponse::bad_request("could not sign ID token"),
    };
    HttpResponse::json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token,
        "refresh_token": format!("fixture-refresh-{}", Uuid::new_v4())
    }))
}

fn query_map(query: Option<&str>) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

fn form_map(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch")
        .as_secs()
}

async fn write_response<S>(
    stream: &mut S,
    response: HttpResponse,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    S: AsyncWriteExt + Unpin,
{
    let reason = match response.status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let mut bytes = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason,
        response.body.len()
    )
    .into_bytes();
    for (name, value) in response.headers {
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(value.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(&response.body);
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}

struct Cli {
    port: u16,
    output_dir: Option<PathBuf>,
}

fn parse_cli() -> Result<Option<Cli>, String> {
    let mut port = 8443;
    let mut output_dir = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "--port" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--port requires a TCP port".to_owned())?;
                port = value
                    .parse()
                    .map_err(|_| format!("invalid TCP port {value:?}"))?;
            }
            "--output-dir" => {
                output_dir =
                    Some(PathBuf::from(arguments.next().ok_or_else(|| {
                        "--output-dir requires a directory".to_owned()
                    })?));
            }
            other => return Err(format!("unknown argument {other:?}; use --help")),
        }
    }
    Ok(Some(Cli { port, output_dir }))
}

fn print_help() {
    println!(
        "Usage: cargo run -p lumvise-resource-server --example oidc_fixture -- [--port PORT] [--output-dir PATH]\n\
         \n\
         Serves a local HTTPS OIDC Authorization Code + PKCE fixture on 127.0.0.1.\n\
         It prints CA and server certificate/key paths plus environment values for the\n\
         supervised resource-server/desktop smoke. The default port is 8443.\n\
         --output-dir keeps TLS artifacts at PATH; otherwise a temporary directory is used\n\
         for the lifetime of the fixture process."
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let Some(cli) = parse_cli().map_err(|error| format!("{error}\nuse --help for usage"))? else {
        return Ok(());
    };
    let temporary_directory;
    let artifact_directory = if let Some(output_dir) = cli.output_dir {
        output_dir
    } else {
        temporary_directory = tempfile::Builder::new()
            .prefix("lumvise-oidc-fixture-")
            .tempdir()?;
        temporary_directory.path().to_path_buf()
    };
    let fixture = Fixture::bind(cli.port, &artifact_directory).await?;
    fixture.info.print_environment();
    let (_shutdown_sender, shutdown_receiver) = oneshot::channel();
    fixture.run(shutdown_receiver).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation, decode, jwk::JwkSet};
    use lumvise_resource_routing::InvocationControl;
    use lumvise_resource_server::{
        AccessTokenValidator, OidcAccessTokenValidator, ServerOidcConfig,
    };

    #[tokio::test]
    async fn serves_trusted_discovery_jwks_and_pkce_token_exchange()
    -> Result<(), Box<dyn Error + Send + Sync>> {
        let artifacts = tempfile::tempdir()?;
        let fixture = Fixture::bind(0, artifacts.path()).await?;
        let info = fixture.info.clone();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let server = tokio::spawn(fixture.run(shutdown_receiver));

        let ca_pem = tokio::fs::read(&info.ca_certificate_path).await?;
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(&ca_pem)?)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let discovery: Value = client
            .get(format!(
                "{}/.well-known/openid-configuration",
                info.issuer_url
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(discovery["issuer"], info.issuer_url);
        assert_eq!(
            discovery["code_challenge_methods_supported"],
            json!(["S256"])
        );

        let jwks: JwkSet = client
            .get(
                discovery["jwks_uri"]
                    .as_str()
                    .expect("fixture supplies JWKS URI"),
            )
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let jwk = jwks
            .find(KEY_ID)
            .expect("fixture JWKS contains signing key");
        let verifier = "fixture-pkce-verifier-used-only-by-this-test";
        let authorize = client
            .get(format!(
                "{}/authorize?response_type=code&client_id=desktop&redirect_uri={}&state=csrf-state&nonce=nonce-value&code_challenge={}&code_challenge_method=S256",
                info.issuer_url,
                url::form_urlencoded::byte_serialize(b"http://127.0.0.1:32145/callback").collect::<String>(),
                pkce_challenge(verifier),
            ))
            .send()
            .await?;
        assert_eq!(authorize.status(), reqwest::StatusCode::FOUND);
        let callback = Url::parse(
            authorize
                .headers()
                .get(reqwest::header::LOCATION)
                .expect("authorization response redirects to callback")
                .to_str()?,
        )?;
        assert_eq!(
            query_map(callback.query()).get("state"),
            Some(&"csrf-state".to_owned())
        );
        let code = query_map(callback.query())
            .remove("code")
            .expect("authorization callback has code");

        let token: Value = client
            .post(
                discovery["token_endpoint"]
                    .as_str()
                    .expect("fixture supplies token endpoint"),
            )
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", "http://127.0.0.1:32145/callback"),
                ("client_id", "desktop"),
                ("code_verifier", verifier),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut validation = Validation::new(Algorithm::ES256);
        validation.set_issuer(&[&info.issuer_url]);
        validation.set_audience(&[ACCESS_TOKEN_AUDIENCE]);
        validation.set_required_spec_claims(&["iss", "aud", "exp", "sub"]);
        let verified = decode::<Value>(
            token["access_token"]
                .as_str()
                .expect("token response has access token"),
            &DecodingKey::from_jwk(jwk)?,
            &validation,
        )?;
        assert_eq!(verified.claims["tenant_id"], "tenant-a");
        assert_eq!(verified.claims["scope"], ACCESS_SCOPE);
        let no_trust_anchor = ServerOidcConfig {
            issuer_url: Url::parse(&info.issuer_url)?,
            ca_certificate_path: None,
            audience: ACCESS_TOKEN_AUDIENCE.into(),
            tenant_claim: "tenant_id".into(),
            required_scope: "lumvise.resources".into(),
        };
        let untrusted_discovery = tokio::task::spawn_blocking(move || {
            OidcAccessTokenValidator::discover(&no_trust_anchor)
        })
        .await?;
        assert!(
            untrusted_discovery.is_err(),
            "the generated CA must not be accepted without its explicit trust anchor"
        );

        let trusted_config = ServerOidcConfig {
            issuer_url: Url::parse(&info.issuer_url)?,
            ca_certificate_path: Some(info.ca_certificate_path.clone()),
            audience: ACCESS_TOKEN_AUDIENCE.into(),
            tenant_claim: "tenant_id".into(),
            required_scope: "lumvise.resources".into(),
        };
        let access_token = token["access_token"]
            .as_str()
            .expect("token response has access token")
            .to_owned();
        let principal = tokio::task::spawn_blocking(move || {
            let validator = OidcAccessTokenValidator::discover(&trusted_config)
                .expect("server discovers fixture with the generated CA");
            validator
                .validate(&access_token, &InvocationControl::sixty_seconds())
                .expect("server validates fixture access token")
        })
        .await?;
        assert_eq!(principal.tenant_id, "tenant-a");
        assert!(principal.scopes.contains("lumvise.resources.migrate"));

        shutdown_sender
            .send(())
            .expect("fixture server remains alive");
        server.await??;
        Ok(())
    }
}
