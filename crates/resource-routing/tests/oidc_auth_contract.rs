use std::{
    collections::BTreeSet,
    io::Write,
    net::TcpStream,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};
use lumvise_resource_routing::{
    InvocationControl, OidcClientConfig, ResourcePlacement, ResourceRoutingConfig,
    auth::{
        CallbackReceiver, JwksResolver, JwtValidationConfig, JwtValidator,
        LoopbackCallbackReceiver, OidcCallback, OidcClient, OidcJwksResolver, OidcLoginAttempt,
        OidcValidationError, RefreshTokenStore,
    },
};
use parking_lot::Mutex;
use serde_json::json;
use url::Url;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const ROUTE_VARS: [&str; 9] = [
    "LUMVISE_ROUTE_LLM_EXECUTION",
    "LUMVISE_ROUTE_SPEECH_INFERENCE",
    "LUMVISE_ROUTE_GRAPH_PERSISTENCE",
    "LUMVISE_ROUTE_SQL_PERSISTENCE",
    "LUMVISE_CENTRAL_SERVER_URL",
    "LUMVISE_OIDC_ISSUER_URL",
    "LUMVISE_OIDC_CLIENT_ID",
    "LUMVISE_OIDC_AUDIENCE",
    "LUMVISE_OIDC_SCOPES",
];

fn clear_environment() {
    for variable in ROUTE_VARS {
        unsafe { std::env::remove_var(variable) };
    }
}

#[test]
fn routes_default_internal_and_centralized_routes_fail_closed() {
    let _lock = ENV_LOCK.lock();
    clear_environment();
    let config = ResourceRoutingConfig::from_environment().unwrap();
    assert_eq!(config.llm_execution, ResourcePlacement::Internal);
    assert!(config.central.is_none());

    unsafe { std::env::set_var("LUMVISE_ROUTE_LLM_EXECUTION", "centralized") };
    assert!(ResourceRoutingConfig::from_environment().is_err());
    unsafe {
        std::env::set_var("LUMVISE_CENTRAL_SERVER_URL", "http://resource.test");
        std::env::set_var("LUMVISE_OIDC_ISSUER_URL", "https://issuer.test");
        std::env::set_var("LUMVISE_OIDC_CLIENT_ID", "desktop");
        std::env::set_var("LUMVISE_OIDC_AUDIENCE", "resources");
        std::env::set_var(
            "LUMVISE_OIDC_SCOPES",
            "openid offline_access lumvise.resources",
        );
    }
    assert!(ResourceRoutingConfig::from_environment().is_err());
    unsafe { std::env::set_var("LUMVISE_CENTRAL_SERVER_URL", "https://resource.test") };
    assert!(ResourceRoutingConfig::from_environment().is_ok());
    unsafe { std::env::set_var("LUMVISE_ROUTE_LLM_EXECUTION", "unknown") };
    assert!(ResourceRoutingConfig::from_environment().is_err());
    clear_environment();
}

#[test]
fn pkce_state_refresh_storage_and_shared_control_are_reusable() {
    let store = Arc::new(RefreshTokenStore::default());
    let client = OidcClient::new(oidc_config(), store);
    client.save_refresh_token("refresh-only").unwrap();
    assert_eq!(
        client.stored_refresh_token().unwrap().as_deref(),
        Some("refresh-only")
    );
    let login = OidcLoginAttempt::new(
        "https://issuer.test/authorize",
        "desktop",
        "http://127.0.0.1:12345/callback",
        &[
            "openid".into(),
            "offline_access".into(),
            "lumvise.resources".into(),
        ],
    )
    .unwrap();
    assert!(
        login
            .authorization_url
            .contains("code_challenge_method=S256")
    );
    assert!(
        login
            .validate_callback(&OidcCallback {
                state: "wrong".into(),
                code: "code".into()
            })
            .is_err()
    );
    let control = InvocationControl::sixty_seconds();
    let child = control.child();
    client.with_refresh_lock(&child, || Ok(())).unwrap();
    assert_eq!(control.deadline(), child.deadline());
    assert_eq!(control.deadline_unix_ms(), child.deadline_unix_ms());
}

#[test]
fn ephemeral_loopback_receiver_accepts_only_a_real_local_callback() {
    let (receiver, redirect_uri) = LoopbackCallbackReceiver::bind().unwrap();
    let address = redirect_uri
        .trim_start_matches("http://")
        .trim_end_matches("/callback")
        .to_owned();
    std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(b"GET /callback?state=state&code=code HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
    });
    assert_eq!(
        receiver
            .receive(&InvocationControl::sixty_seconds())
            .unwrap(),
        OidcCallback {
            state: "state".into(),
            code: "code".into()
        }
    );
}

#[test]
fn unknown_jwks_key_refreshes_once_before_signed_validation() {
    let validator = JwtValidator::new(JwtValidationConfig {
        issuer: "https://issuer.test".into(),
        audience: "resources".into(),
        tenant_claim: "tenant_id".into(),
        required_scope: "lumvise.resources".into(),
        advertised_algorithms: vec![Algorithm::HS256],
    })
    .unwrap();
    let key = EncodingKey::from_secret(b"test-signing-key");
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("rotated".into());
    let token = issue_with_header(
        &header,
        &key,
        json!({"tenant_id":"tenant-a", "scope":"lumvise.resources"}),
    );
    let resolver = RotatingResolver {
        refreshes: AtomicUsize::new(0),
    };
    let principal = validator
        .validate_with_jwks(&token, &resolver, &InvocationControl::sixty_seconds())
        .unwrap();
    assert_eq!(principal.tenant_id, "tenant-a");
    assert_eq!(resolver.refreshes.load(Ordering::Relaxed), 1);
}

#[test]
fn signed_jwt_requires_issuer_audience_scope_and_string_tenant() {
    let validator = JwtValidator::new(JwtValidationConfig {
        issuer: "https://issuer.test".into(),
        audience: "resources".into(),
        tenant_claim: "tenant_id".into(),
        required_scope: "lumvise.resources".into(),
        advertised_algorithms: vec![Algorithm::HS256],
    })
    .unwrap();
    let key = EncodingKey::from_secret(b"test-signing-key");
    let token = issue(
        &key,
        json!({"tenant_id":"tenant-a", "scope":"lumvise.resources"}),
    );
    let principal = validator
        .validate_with_key(&token, &DecodingKey::from_secret(b"test-signing-key"))
        .unwrap();
    assert_eq!(principal.tenant_id, "tenant-a");
    assert_eq!(
        principal.scopes,
        BTreeSet::from(["lumvise.resources".into()])
    );

    let no_tenant = issue(&key, json!({"tenant_id": 7, "scope":"lumvise.resources"}));
    assert!(
        validator
            .validate_with_key(&no_tenant, &DecodingKey::from_secret(b"test-signing-key"))
            .is_err()
    );
    let no_scope = issue(&key, json!({"tenant_id": "tenant-a"}));
    assert!(
        validator
            .validate_with_key(&no_scope, &DecodingKey::from_secret(b"test-signing-key"))
            .is_err()
    );
    let id_token = issue(
        &key,
        json!({"tenant_id":"tenant-a", "scope":"lumvise.resources", "nonce":"id-token"}),
    );
    assert!(
        validator
            .validate_with_key(&id_token, &DecodingKey::from_secret(b"test-signing-key"))
            .is_err()
    );
}

struct RotatingResolver {
    refreshes: AtomicUsize,
}

impl JwksResolver for RotatingResolver {
    fn key_for(
        &self,
        kid: &str,
        refresh: bool,
        _: &InvocationControl,
    ) -> Result<DecodingKey, OidcValidationError> {
        if !refresh {
            return Err(OidcValidationError::UnknownKeyId(kid.into()));
        }
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        Ok(DecodingKey::from_secret(b"test-signing-key"))
    }
}

fn oidc_config() -> OidcClientConfig {
    OidcClientConfig {
        issuer_url: Url::parse("https://issuer.test").unwrap(),
        client_id: "desktop".into(),
        audience: "resources".into(),
        scopes: vec!["openid".into()],
        ca_certificate_path: None,
    }
}

fn issue(key: &EncodingKey, extra: serde_json::Value) -> String {
    issue_with_header(&Header::new(Algorithm::HS256), key, extra)
}

fn issue_with_header(header: &Header, key: &EncodingKey, extra: serde_json::Value) -> String {
    let mut claims = serde_json::Map::new();
    claims.insert("iss".into(), json!("https://issuer.test"));
    claims.insert("aud".into(), json!("resources"));
    claims.insert("sub".into(), json!("subject-a"));
    claims.insert("exp".into(), json!(4_102_444_800_u64));
    claims.extend(extra.as_object().unwrap().clone());
    encode(header, &claims, key).unwrap()
}

/// Verifies that a self-signed OIDC fixture is rejected without its CA and
/// accepted when the CA certificate is configured as an additional trust anchor.
#[tokio::test]
async fn ca_certificate_trusts_configured_issuer_and_rejects_untrusted() {
    // Generate a self-signed CA and a server certificate for 127.0.0.1.
    let artifacts = tempfile::tempdir().unwrap();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "OIDC test CA");
    let ca_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let ca_certificate = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let mut server_params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    server_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "OIDC test server");
    let server_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let server_certificate = server_params.signed_by(&server_key, &issuer).unwrap();

    let ca_pem_path = artifacts.path().join("test-ca.pem");
    std::fs::write(&ca_pem_path, ca_certificate.pem()).unwrap();
    let server_certificate_der = server_certificate.der().clone();
    let server_private_key = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()),
    );

    // Pre-generate a signing key and JWKS for the fixture.
    let fixture_signing_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let fixture_encoding_key =
        jsonwebtoken::EncodingKey::from_ec_der(&fixture_signing_key.serialize_der());
    let mut jwk = serde_json::to_value(
        jsonwebtoken::jwk::Jwk::from_encoding_key(
            &fixture_encoding_key,
            jsonwebtoken::Algorithm::ES256,
        )
        .unwrap(),
    )
    .unwrap();
    let jwk_obj = jwk.as_object_mut().unwrap();
    jwk_obj.insert("kid".into(), serde_json::Value::String("test-key".into()));
    jwk_obj.insert("use".into(), serde_json::Value::String("sig".into()));
    jwk_obj.insert("alg".into(), serde_json::Value::String("ES256".into()));
    let jwks = serde_json::json!({"keys": [jwk]});

    // Bind the TCP listener and build the TLS acceptor.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let bound_port = listener.local_addr().unwrap().port();
    let issuer_url = format!("https://127.0.0.1:{bound_port}");

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![server_certificate_der], server_private_key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls_config));

    // Spawn the fixture server.
    let jwks_clone = jwks.clone();
    let issuer_clone = issuer_url.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let jwks = jwks_clone.clone();
            let issuer = issuer_clone.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let n = tls.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let (status, body) = if request.contains("/.well-known/openid-configuration") {
                        let body = serde_json::json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/authorize"),
                            "token_endpoint": format!("{issuer}/token"),
                            "jwks_uri": format!("{issuer}/.well-known/jwks"),
                            "id_token_signing_alg_values_supported": ["ES256"],
                            "code_challenge_methods_supported": ["S256"],
                        });
                        (200, serde_json::to_string(&body).unwrap())
                    } else if request.contains("/.well-known/jwks") {
                        (200, serde_json::to_string(&jwks).unwrap())
                    } else {
                        (404, "Not Found".into())
                    };
                    let response = format!(
                        "HTTP/1.1 {status} OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tls.write_all(response.as_bytes()).await;
                }
            });
        }
    });

    // Attempt discovery without the CA — must fail (untrusted fixture).
    let control = lumvise_resource_routing::control::InvocationControl::sixty_seconds();
    let issuer_url_for_untrusted = issuer_url.clone();
    let untrusted = tokio::task::spawn_blocking(move || {
        OidcJwksResolver::discover(&issuer_url_for_untrusted, &control)
    })
    .await
    .unwrap();
    assert!(
        untrusted.is_err(),
        "self-signed fixture must be rejected without the CA certificate"
    );

    // Discovery with the CA — must succeed.
    let control = lumvise_resource_routing::control::InvocationControl::sixty_seconds();
    let issuer_url_for_trusted = issuer_url.clone();
    let ca_pem_path_for_trusted = ca_pem_path.clone();
    let trusted = tokio::task::spawn_blocking(move || {
        OidcJwksResolver::discover_with_ca_certificate_path(
            &issuer_url_for_trusted,
            Some(&ca_pem_path_for_trusted),
            &control,
        )
    })
    .await
    .unwrap();

    let resolver = trusted.expect("discovery with CA certificate must succeed");
    assert_eq!(resolver.issuer(), issuer_url.trim_end_matches('/'));
    assert!(!resolver.advertised_algorithms().is_empty());
}

/// The assistant-e2e launcher performs the fixture's verified HTTPS
/// authorization request, then lets the existing loopback receiver consume the
/// actual redirect rather than manufacturing a callback.
#[cfg(feature = "assistant-e2e")]
#[tokio::test]
async fn fixture_browser_launcher_follows_verified_authorization_redirect_to_loopback() {
    use lumvise_resource_routing::auth::{BrowserLauncher, FixtureBrowserLauncher};

    let artifacts = tempfile::tempdir().unwrap();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut certificate_params =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    certificate_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "assistant-e2e OIDC fixture");
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let certificate = certificate_params.self_signed(&key).unwrap();
    let ca_path = artifacts.path().join("fixture-ca.pem");
    std::fs::write(&ca_path, certificate.pem()).unwrap();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                key.serialize_der(),
            )),
        )
        .unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut tls = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config))
            .accept(stream)
            .await
            .unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buffer = vec![0; 4096];
        let bytes = tls.read(&mut buffer).await.unwrap();
        let target = String::from_utf8_lossy(&buffer[..bytes])
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let request_url = Url::parse(&format!("https://127.0.0.1:{port}{target}")).unwrap();
        let redirect_uri = request_url
            .query_pairs()
            .find(|(name, _)| name == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        let state = request_url
            .query_pairs()
            .find(|(name, _)| name == "state")
            .unwrap()
            .1
            .into_owned();
        let mut callback = Url::parse(&redirect_uri).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("code", "fixture-authorization-code")
            .append_pair("state", &state);
        let response =
            format!("HTTP/1.1 302 Found\r\nlocation: {callback}\r\ncontent-length: 0\r\n\r\n");
        tls.write_all(response.as_bytes()).await.unwrap();
    });

    let (receiver, redirect_uri) = LoopbackCallbackReceiver::bind().unwrap();
    let authorization_url = format!(
        "https://127.0.0.1:{port}/authorize?redirect_uri={}&state=generated-state",
        url::form_urlencoded::byte_serialize(redirect_uri.as_bytes()).collect::<String>(),
    );
    FixtureBrowserLauncher::new(ca_path)
        .open(&authorization_url)
        .unwrap();
    let callback = receiver
        .receive(&InvocationControl::sixty_seconds())
        .unwrap();

    assert_eq!(callback.state, "generated-state");
    assert_eq!(callback.code, "fixture-authorization-code");
}
