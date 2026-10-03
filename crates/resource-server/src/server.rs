use std::{
    io::BufReader,
    path::Path,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use h2::server;
use http::{Request, Response, StatusCode, header};
use lumvise_neural_core::{LlmProviderRegistry, SpeechRecognizer, SpeechSynthesizer};
#[cfg(not(feature = "assistant-e2e"))]
use lumvise_neural_core::{
    Text2VoiceService, Voice2TextService, llm_providers::http_client::ReqwestLlmHttpClient,
};
use lumvise_resource_routing::{
    InvocationControl,
    auth::{AuthenticatedPrincipal, JwtValidationConfig, JwtValidator, OidcJwksResolver},
    protocol::{
        CONTENT_TYPE, InvocationEnvelopeV1, InvocationTerminalStatusV1, InvocationTerminalV1,
        PROTOCOL_MAJOR, ReadinessRequestV1, SequenceValidator, decode_frame, encode_frame,
        invocation_envelope_v1::Payload,
    },
    transport::{CANCEL_PATH, INVOKE_PATH, READINESS_PATH},
};
use prost::Message;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use crate::{
    config::{ResourceServerConfig, ServerNeuralConfig, ServerOidcConfig},
    dispatch::{
        CentralizedPersistenceProtocolCodec, DispatchError, DispatchResources,
        PersistenceProtocolCodec, ResourceDispatcher,
    },
    tenant::{
        LocalTenantPersistenceFactory, PrincipalPersistenceFactory, TenantAdapterCache,
        TenantPersistenceFactory,
    },
};

pub trait AccessTokenValidator: Send + Sync {
    fn validate(
        &self,
        access_token: &str,
        control: &InvocationControl,
    ) -> Result<AuthenticatedPrincipal, String>;
}

pub struct OidcAccessTokenValidator {
    validator: JwtValidator,
    resolver: OidcJwksResolver,
}

impl OidcAccessTokenValidator {
    pub fn discover(config: &ServerOidcConfig) -> Result<Self, ResourceServerError> {
        let control = InvocationControl::sixty_seconds();
        let resolver = OidcJwksResolver::discover_with_ca_certificate_path(
            config.issuer_url.as_str(),
            config.ca_certificate_path.as_deref(),
            &control,
        )
        .map_err(|error| ResourceServerError::AuthenticationSetup(error.to_string()))?;
        let validator = JwtValidator::new(JwtValidationConfig {
            issuer: resolver.issuer().to_owned(),
            audience: config.audience.clone(),
            tenant_claim: config.tenant_claim.clone(),
            required_scope: config.required_scope.clone(),
            advertised_algorithms: resolver.advertised_algorithms().to_vec(),
        })
        .map_err(|error| ResourceServerError::AuthenticationSetup(error.to_string()))?;
        Ok(Self {
            validator,
            resolver,
        })
    }
}

impl AccessTokenValidator for OidcAccessTokenValidator {
    fn validate(
        &self,
        access_token: &str,
        control: &InvocationControl,
    ) -> Result<AuthenticatedPrincipal, String> {
        self.validator
            .validate_with_jwks(access_token, &self.resolver, control)
            .map_err(|error| error.to_string())
    }
}

/// Only owning internal adapters are accepted here. `ResourceServer` constructs
/// tenant-local persistence itself and never accepts centralized adapters.
pub struct ServerResources {
    pub http_extension: Option<Arc<dyn crate::ResourceHttpExtension>>,
    pub data_dir: std::path::PathBuf,
    pub access_tokens: Arc<dyn AccessTokenValidator>,
    pub llm_registry: Option<Arc<Mutex<LlmProviderRegistry>>>,
    pub speech_recognizer: Option<Arc<dyn SpeechRecognizer>>,
    pub speech_synthesizer: Option<Arc<dyn SpeechSynthesizer>>,
    pub tenant_factory: Arc<dyn TenantPersistenceFactory>,
    pub principal_factory: Option<Arc<dyn PrincipalPersistenceFactory>>,
    pub persistence_codec: Arc<dyn PersistenceProtocolCodec>,
}

impl ServerResources {
    pub fn internal(
        data_dir: std::path::PathBuf,
        access_tokens: Arc<dyn AccessTokenValidator>,
        llm_registry: Option<Arc<Mutex<LlmProviderRegistry>>>,
        speech_recognizer: Option<Arc<dyn SpeechRecognizer>>,
        speech_synthesizer: Option<Arc<dyn SpeechSynthesizer>>,
    ) -> Self {
        Self {
            http_extension: None,
            data_dir,
            access_tokens,
            llm_registry,
            speech_recognizer,
            speech_synthesizer,
            tenant_factory: Arc::new(LocalTenantPersistenceFactory),
            principal_factory: None,
            persistence_codec: Arc::new(CentralizedPersistenceProtocolCodec),
        }
    }

    /// Constructs only server-owned Neural Core adapters. This is the binary
    /// composition root; centralized and desktop adapters cannot enter here.
    pub fn internal_from_neural_configuration(
        data_dir: std::path::PathBuf,
        access_tokens: Arc<dyn AccessTokenValidator>,
        neural: &ServerNeuralConfig,
    ) -> Result<Self, ResourceServerError> {
        let (llm_registry, speech_recognizer, speech_synthesizer) =
            configured_neural_adapters(neural)?;
        Ok(Self::internal(
            data_dir,
            access_tokens,
            llm_registry,
            speech_recognizer,
            speech_synthesizer,
        ))
    }
}

#[cfg(not(feature = "assistant-e2e"))]
fn configured_neural_adapters(
    neural: &ServerNeuralConfig,
) -> Result<
    (
        Option<Arc<Mutex<LlmProviderRegistry>>>,
        Option<Arc<dyn SpeechRecognizer>>,
        Option<Arc<dyn SpeechSynthesizer>>,
    ),
    ResourceServerError,
> {
    let llm_registry = neural
        .llm
        .clone()
        .map(|config| {
            LlmProviderRegistry::from_configs(vec![config], Arc::new(ReqwestLlmHttpClient::new()))
                .map(|registry| Arc::new(Mutex::new(registry)))
                .map_err(|error| {
                    ResourceServerError::Protocol(format!("building internal LLM: {error}"))
                })
        })
        .transpose()?;
    let speech_recognizer = neural
        .speech_recognizer
        .clone()
        .map(|config| {
            Voice2TextService::new(config)
                .map(|service| Arc::new(service) as Arc<dyn SpeechRecognizer>)
                .map_err(|error| {
                    ResourceServerError::Protocol(format!(
                        "building internal speech recognition: {error}"
                    ))
                })
        })
        .transpose()?;
    let speech_synthesizer = neural
        .speech_synthesizer
        .clone()
        .map(|config| {
            Text2VoiceService::new(config)
                .map(|service| Arc::new(service) as Arc<dyn SpeechSynthesizer>)
                .map_err(|error| {
                    ResourceServerError::Protocol(format!(
                        "building internal speech synthesis: {error}"
                    ))
                })
        })
        .transpose()?;
    Ok((llm_registry, speech_recognizer, speech_synthesizer))
}

#[cfg(feature = "assistant-e2e")]
fn configured_neural_adapters(
    _: &ServerNeuralConfig,
) -> Result<
    (
        Option<Arc<Mutex<LlmProviderRegistry>>>,
        Option<Arc<dyn SpeechRecognizer>>,
        Option<Arc<dyn SpeechSynthesizer>>,
    ),
    ResourceServerError,
> {
    let registry = lumvise_neural_core::assistant_e2e::llm_registry().map_err(|error| {
        ResourceServerError::Protocol(format!("building assistant-e2e internal LLM: {error}"))
    })?;
    let (recognizer, synthesizer) = lumvise_neural_core::assistant_e2e::speech_adapters();
    Ok((
        Some(Arc::new(Mutex::new(registry))),
        Some(Arc::new(recognizer)),
        Some(Arc::new(synthesizer)),
    ))
}

pub struct ResourceServer {
    http_extension: Option<Arc<dyn crate::ResourceHttpExtension>>,
    access_tokens: Arc<dyn AccessTokenValidator>,
    dispatcher: Arc<ResourceDispatcher>,
}

impl ResourceServer {
    pub fn new(resources: ServerResources) -> Self {
        let tenants = Arc::new(TenantAdapterCache::new(
            resources.data_dir,
            resources.tenant_factory,
        ));
        let dispatcher = Arc::new(ResourceDispatcher::new(DispatchResources {
            llm_registry: resources.llm_registry,
            speech_recognizer: resources.speech_recognizer,
            speech_synthesizer: resources.speech_synthesizer,
            tenants,
            principal_factory: resources.principal_factory,
            persistence_codec: resources.persistence_codec,
        }));
        Self {
            http_extension: resources.http_extension,
            access_tokens: resources.access_tokens,
            dispatcher,
        }
    }

    pub fn dispatcher(&self) -> &ResourceDispatcher {
        self.dispatcher.as_ref()
    }

    pub async fn serve(
        self: Arc<Self>,
        config: &ResourceServerConfig,
    ) -> Result<(), ResourceServerError> {
        let tls = TlsAcceptor::from(Arc::new(load_tls_config(
            &config.tls_certificate_path,
            &config.tls_key_path,
        )?));
        let listener = TcpListener::bind(config.bind).await?;
        loop {
            let (tcp, _) = listener.accept().await?;
            let server = Arc::clone(&self);
            let tls = tls.clone();
            tokio::spawn(async move {
                if let Err(error) = server.serve_connection(tls, tcp).await {
                    tracing::debug!(error = %error, "resource server connection closed");
                }
            });
        }
    }

    async fn serve_connection(
        self: Arc<Self>,
        tls: TlsAcceptor,
        tcp: TcpStream,
    ) -> Result<(), ResourceServerError> {
        let tls = tls.accept(tcp).await?;
        let mut connection = server::handshake(tls).await?;
        while let Some(result) = connection.accept().await {
            let (request, respond) = result?;
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                let _ = server.handle(request, respond).await;
            });
        }
        Ok(())
    }

    async fn handle(
        &self,
        request: Request<h2::RecvStream>,
        mut respond: h2::server::SendResponse<Bytes>,
    ) -> Result<(), ResourceServerError> {
        let path = request.uri().path().to_owned();
        if let Some(extension) = self
            .http_extension
            .as_ref()
            .filter(|extension| extension.accepts(&path))
        {
            return self
                .handle_extension(extension.clone(), request, respond)
                .await;
        }
        let terminal_error = |message: String| InvocationTerminalV1 {
            status: InvocationTerminalStatusV1::InvalidArgument as i32,
            retryable: false,
            outcome_unknown: false,
            error_code: Some("transport_validation".into()),
            message: Some(message),
            result: None,
        };
        if request.method() != http::Method::POST
            || !matches!(path.as_str(), READINESS_PATH | INVOKE_PATH | CANCEL_PATH)
        {
            send_terminal(
                &mut respond,
                StatusCode::NOT_FOUND,
                String::new(),
                terminal_error("unknown resource route".into()),
            )
            .await?;
            return Ok(());
        }
        if request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some(CONTENT_TYPE)
        {
            send_terminal(
                &mut respond,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                String::new(),
                terminal_error("resource protocol content type is required".into()),
            )
            .await?;
            return Ok(());
        }
        if request
            .headers()
            .get("lumvise-resource-protocol-major")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u32>().ok())
            != Some(PROTOCOL_MAJOR)
        {
            send_terminal(
                &mut respond,
                StatusCode::BAD_REQUEST,
                String::new(),
                terminal_error("unsupported protocol major".into()),
            )
            .await?;
            return Ok(());
        }
        let access_token = match bearer(request.headers()) {
            Ok(token) => token,
            Err(message) => {
                send_terminal(
                    &mut respond,
                    StatusCode::UNAUTHORIZED,
                    String::new(),
                    terminal_error(message),
                )
                .await?;
                return Ok(());
            }
        };
        let auth_control = InvocationControl::sixty_seconds();
        let validator = Arc::clone(&self.access_tokens);
        let access_token = access_token.to_owned();
        let principal = match tokio::task::spawn_blocking(move || {
            validator.validate(&access_token, &auth_control)
        })
        .await
        .map_err(|error| {
            ResourceServerError::AuthenticationSetup(format!(
                "access-token validation task failed: {error}"
            ))
        })? {
            Ok(principal) => principal,
            Err(message) => {
                send_terminal(
                    &mut respond,
                    StatusCode::UNAUTHORIZED,
                    String::new(),
                    terminal_error(format!("access token rejected: {message}")),
                )
                .await?;
                return Ok(());
            }
        };
        let body = collect_body(request.into_body()).await?;
        match path.as_str() {
            READINESS_PATH => {
                let request = match decode_frame::<ReadinessRequestV1>(&body) {
                    Ok(request) => request,
                    Err(error) => {
                        send_terminal(
                            &mut respond,
                            StatusCode::BAD_REQUEST,
                            String::new(),
                            terminal_error(error.to_string()),
                        )
                        .await?;
                        return Ok(());
                    }
                };
                match self.dispatcher.readiness(&principal, request) {
                    Ok(readiness) => send_message(&mut respond, StatusCode::OK, &readiness).await?,
                    Err(error) => {
                        send_terminal(
                            &mut respond,
                            StatusCode::BAD_REQUEST,
                            String::new(),
                            dispatch_terminal(error),
                        )
                        .await?
                    }
                }
            }
            INVOKE_PATH => {
                let frames = match decode_envelopes(&body) {
                    Ok(frames) => frames,
                    Err(error) => {
                        send_terminal(
                            &mut respond,
                            StatusCode::BAD_REQUEST,
                            String::new(),
                            terminal_error(error.to_string()),
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let dispatcher = Arc::clone(&self.dispatcher);
                let responses = tokio::task::spawn_blocking(move || {
                    dispatcher.invoke_envelopes(&principal, frames)
                })
                .await
                .map_err(|error| {
                    ResourceServerError::Protocol(format!("dispatch task failed: {error}"))
                })?;
                send_envelopes(&mut respond, StatusCode::OK, &responses).await?;
            }
            CANCEL_PATH => {
                let frames = match decode_envelopes(&body) {
                    Ok(frames) => frames,
                    Err(error) => {
                        send_terminal(
                            &mut respond,
                            StatusCode::BAD_REQUEST,
                            String::new(),
                            terminal_error(error.to_string()),
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let Some(frame) = frames.first() else {
                    send_terminal(
                        &mut respond,
                        StatusCode::BAD_REQUEST,
                        String::new(),
                        terminal_error("cancel request has no envelope".into()),
                    )
                    .await?;
                    return Ok(());
                };
                if frames.len() != 1
                    || frame.protocol_minor != lumvise_resource_routing::protocol::PROTOCOL_MINOR
                {
                    send_terminal(
                        &mut respond,
                        StatusCode::BAD_REQUEST,
                        frame.request_id.clone(),
                        terminal_error(
                            "cancel request has unsupported minor or extra frames".into(),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                let mut sequence = SequenceValidator::new(&frame.request_id);
                if let Err(error) = sequence.validate(frame) {
                    send_terminal(
                        &mut respond,
                        StatusCode::BAD_REQUEST,
                        frame.request_id.clone(),
                        terminal_error(error.to_string()),
                    )
                    .await?;
                    return Ok(());
                }
                let client_instance_id = match frame.payload.as_ref() {
                    Some(Payload::Cancel(cancel))
                        if !cancel.client_instance_id.trim().is_empty() =>
                    {
                        cancel.client_instance_id.as_str()
                    }
                    _ => {
                        send_terminal(
                            &mut respond,
                            StatusCode::BAD_REQUEST,
                            frame.request_id.clone(),
                            terminal_error("cancel request requires a client instance ID".into()),
                        )
                        .await?;
                        return Ok(());
                    }
                };
                self.dispatcher
                    .cancel(&principal, client_instance_id, &frame.request_id);
                send_terminal(
                    &mut respond,
                    StatusCode::OK,
                    frame.request_id.clone(),
                    InvocationTerminalV1 {
                        status: InvocationTerminalStatusV1::Cancelled as i32,
                        retryable: false,
                        outcome_unknown: false,
                        error_code: None,
                        message: None,
                        result: None,
                    },
                )
                .await?;
            }
            _ => unreachable!("route was validated"),
        }
        Ok(())
    }

    async fn handle_extension(
        &self,
        extension: Arc<dyn crate::ResourceHttpExtension>,
        request: Request<h2::RecvStream>,
        mut respond: h2::server::SendResponse<Bytes>,
    ) -> Result<(), ResourceServerError> {
        let control = InvocationControl::sixty_seconds();
        let method = request.method().to_string();
        let path = request.uri().path().to_owned();
        let bearer = bearer(request.headers()).ok().map(str::to_owned);
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = tokio::time::timeout(control.remaining(), collect_body(request.into_body()))
            .await
            .map_err(|_| {
                ResourceServerError::Protocol("HTTP extension body deadline elapsed".into())
            })??;
        let response = tokio::task::spawn_blocking(move || {
            extension.handle(
                crate::ResourceHttpRequest {
                    method,
                    path,
                    bearer,
                    content_type,
                    body,
                },
                &control,
            )
        })
        .await
        .map_err(|error| ResourceServerError::Protocol(error.to_string()))?;
        let headers = Response::builder()
            .status(response.status)
            .header(header::CONTENT_TYPE, response.content_type)
            .header(header::CACHE_CONTROL, "no-store")
            .body(())
            .map_err(|error| ResourceServerError::Protocol(error.to_string()))?;
        respond
            .send_response(headers, false)?
            .send_data(Bytes::from(response.body), true)?;
        Ok(())
    }
}

async fn send_envelopes(
    respond: &mut h2::server::SendResponse<Bytes>,
    status: StatusCode,
    envelopes: &[InvocationEnvelopeV1],
) -> Result<(), ResourceServerError> {
    let body = envelopes.iter().flat_map(encode_frame).collect::<Vec<_>>();
    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, CONTENT_TYPE)
        .body(())
        .expect("fixed response is valid");
    let mut stream = respond.send_response(response, false)?;
    stream.send_data(Bytes::from(body), true)?;
    Ok(())
}

fn bearer(headers: &http::HeaderMap) -> Result<&str, String> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or("Authorization: Bearer token is required")?;
    value
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Authorization must use Bearer access token".into())
}

async fn collect_body(mut stream: h2::RecvStream) -> Result<Vec<u8>, ResourceServerError> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk?;
        stream.flow_control().release_capacity(chunk.len())?;
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn decode_envelopes(body: &[u8]) -> Result<Vec<InvocationEnvelopeV1>, ResourceServerError> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        if body.len() - offset < 8 {
            return Err(ResourceServerError::Protocol(
                "frame ended before length prefix".into(),
            ));
        }
        let declared = u64::from_be_bytes(
            body[offset..offset + 8]
                .try_into()
                .expect("eight byte prefix"),
        );
        let payload_len = usize::try_from(declared).map_err(|_| {
            ResourceServerError::Protocol("frame length cannot fit local usize".into())
        })?;
        let frame_end = offset
            .checked_add(8)
            .and_then(|value| value.checked_add(payload_len))
            .ok_or_else(|| {
                ResourceServerError::Protocol("frame length overflows local address space".into())
            })?;
        if frame_end > body.len() {
            return Err(ResourceServerError::Protocol(
                "frame ended before declared body".into(),
            ));
        }
        frames.push(
            decode_frame(&body[offset..frame_end])
                .map_err(|error| ResourceServerError::Protocol(error.to_string()))?,
        );
        offset = frame_end;
    }
    Ok(frames)
}

async fn send_message<M: Message>(
    respond: &mut h2::server::SendResponse<Bytes>,
    status: StatusCode,
    message: &M,
) -> Result<(), ResourceServerError> {
    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, CONTENT_TYPE)
        .body(())
        .expect("fixed response is valid");
    let mut stream = respond.send_response(response, false)?;
    stream.send_data(Bytes::from(encode_frame(message)), true)?;
    Ok(())
}

async fn send_terminal(
    respond: &mut h2::server::SendResponse<Bytes>,
    status: StatusCode,
    request_id: String,
    terminal: InvocationTerminalV1,
) -> Result<(), ResourceServerError> {
    let envelope = InvocationEnvelopeV1 {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: lumvise_resource_routing::protocol::PROTOCOL_MINOR,
        request_id,
        sequence: 0,
        payload: Some(Payload::Terminal(terminal)),
    };
    send_message(respond, status, &envelope).await
}

fn dispatch_terminal(error: DispatchError) -> InvocationTerminalV1 {
    InvocationTerminalV1 {
        status: InvocationTerminalStatusV1::Failed as i32,
        retryable: false,
        outcome_unknown: false,
        error_code: Some("dispatch".into()),
        message: Some(error.to_string()),
        result: None,
    }
}

fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<ServerConfig, ResourceServerError> {
    let cert_file = std::fs::File::open(cert_path)?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_reader).collect::<Result<_, _>>()?;
    if certs.is_empty() {
        return Err(ResourceServerError::Tls(
            "certificate PEM has no certificate".into(),
        ));
    }
    let key_file = std::fs::File::open(key_path)?;
    let mut key_reader = BufReader::new(key_file);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| ResourceServerError::Tls("key PEM has no private key".into()))?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(config)
}

#[derive(Debug, Error)]
pub enum ResourceServerError {
    #[error("OIDC server authentication setup failed: {0}")]
    AuthenticationSetup(String),
    #[error("resource protocol failed: {0}")]
    Protocol(String),
    #[error("TLS configuration failed: {0}")]
    Tls(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP/2 failed: {0}")]
    H2(#[from] h2::Error),
    #[error("TLS failed: {0}")]
    Rustls(#[from] rustls::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use rcgen::{
        BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
        PKCS_ECDSA_P256_SHA256,
    };

    use super::*;
    use lumvise_resource_routing::{
        AuthenticatedFramedClient, CentralServerConfig, Http2CentralTransport,
        config::OidcClientConfig,
        protocol::{CapabilityReadinessV1, ResourceCapabilityV1},
    };

    struct TestAccessTokenValidator;

    impl AccessTokenValidator for TestAccessTokenValidator {
        fn validate(
            &self,
            access_token: &str,
            _: &InvocationControl,
        ) -> Result<AuthenticatedPrincipal, String> {
            if access_token != "authenticated-test-token" {
                return Err("unexpected test access token".into());
            }
            Ok(AuthenticatedPrincipal {
                issuer: "https://issuer.test".into(),
                subject: "desktop-test".into(),
                tenant_id: "tenant-test".into(),
                scopes: BTreeSet::from(["lumvise.resources".into()]),
            })
        }
    }

    fn future_deadline() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_millis() as u64
            + 59_000
    }

    fn tls_material(
        directory: &Path,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let mut ca_params =
            CertificateParams::new(Vec::<String>::new()).expect("CA parameters are valid");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "resource-server test CA");
        let ca_key =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("CA key generation succeeds");
        let ca_certificate = ca_params
            .self_signed(&ca_key)
            .expect("CA certificate generation succeeds");
        let issuer = Issuer::new(ca_params, ca_key);

        let server_params = CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .expect("server certificate parameters are valid");
        let server_key =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("server key generation succeeds");
        let server_certificate = server_params
            .signed_by(&server_key, &issuer)
            .expect("server certificate generation succeeds");

        let ca_path = directory.join("ca.pem");
        let certificate_path = directory.join("server.pem");
        let key_path = directory.join("server-key.pem");
        std::fs::write(&ca_path, ca_certificate.pem()).expect("writing CA certificate succeeds");
        std::fs::write(&certificate_path, server_certificate.pem())
            .expect("writing server certificate succeeds");
        std::fs::write(&key_path, server_key.serialize_pem()).expect("writing server key succeeds");
        (ca_path, certificate_path, key_path)
    }

    #[tokio::test]
    async fn authenticated_readiness_round_trips_over_tls_h2_before_deadline() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let directory = tempfile::tempdir().expect("temporary test directory is created");
        let (ca_path, certificate_path, key_path) = tls_material(directory.path());
        let server = Arc::new(ResourceServer::new(ServerResources::internal(
            directory.path().join("tenant-data"),
            Arc::new(TestAccessTokenValidator),
            None,
            None,
            None,
        )));
        let acceptor = TlsAcceptor::from(Arc::new(
            load_tls_config(&certificate_path, &key_path).expect("server TLS configuration loads"),
        ));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener binds");
        let address = listener.local_addr().expect("test listener has an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await?;
            server.serve_connection(acceptor, tcp).await
        });

        let central_config = CentralServerConfig {
            url: url::Url::parse(&format!("https://127.0.0.1:{}", address.port()))
                .expect("test resource URL is valid"),
            ca_certificate_path: Some(ca_path),
            oidc: OidcClientConfig {
                issuer_url: url::Url::parse("https://issuer.test")
                    .expect("test issuer URL is valid"),
                client_id: "desktop-test".into(),
                audience: "resources-test".into(),
                scopes: vec!["lumvise.resources".into()],
                ca_certificate_path: None,
            },
        };
        let client = AuthenticatedFramedClient::new(
            Arc::new(
                Http2CentralTransport::from_config(&central_config)
                    .expect("client TLS configuration loads"),
            ),
            "authenticated-test-token",
        );
        let control = InvocationControl::sixty_seconds();
        let request = ReadinessRequestV1 {
            supported_majors: vec![PROTOCOL_MAJOR],
            supported_minors: vec![lumvise_resource_routing::protocol::PROTOCOL_MINOR],
            deadline_unix_ms: future_deadline(),
            client_instance_id: "desktop-test".into(),
            requested_capabilities: vec![ResourceCapabilityV1::GraphPersistence as i32],
        };
        let readiness =
            tokio::time::timeout(Duration::from_secs(2), client.readiness(&request, &control))
                .await
                .expect("readiness must not stall at the H2 request/response boundary")
                .expect("authenticated readiness succeeds");

        assert_eq!(readiness.tenant_id, "tenant-test");
        assert_eq!(readiness.capabilities.len(), 1);
        assert_eq!(
            readiness.capabilities[0].status,
            CapabilityReadinessV1::Ready as i32
        );
        drop(client);

        server_task
            .await
            .expect("resource server task joins")
            .expect("resource server connection completes");
    }
}
