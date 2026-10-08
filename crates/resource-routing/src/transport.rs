use std::{
    future::{Future, poll_fn},
    io::BufReader,
    path::Path,
    sync::Arc,
};

use bytes::Bytes;
use h2::client::SendRequest;
use h2::{SendStream, client::ResponseFuture};
use http::{Request, header};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, ServerName},
};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::{
    config::CentralServerConfig,
    control::InvocationControl,
    protocol::{
        CONTENT_TYPE, InvocationEnvelopeV1, InvocationTerminalV1, PROTOCOL_MAJOR,
        ReadinessRequestV1, ReadinessResponseV1, decode_frame, encode_frame,
    },
};

pub const READINESS_PATH: &str = "/resources/v1/readiness";
pub const INVOKE_PATH: &str = "/resources/v1/invoke";
pub const CANCEL_PATH: &str = "/resources/v1/cancel";

/// Verified-TLS HTTP/2 connector. It intentionally shares no code with the
/// plaintext loopback AppBridge transport.
pub struct Http2CentralTransport {
    origin: Url,
    tls: TlsConnector,
}

impl Http2CentralTransport {
    pub fn from_config(config: &CentralServerConfig) -> Result<Self, TransportError> {
        Self::from_endpoint(config.url.clone(), config.ca_certificate_path.as_deref())
    }

    /// Connects to an authenticated resource endpoint without prescribing its login method.
    /// Example: `Http2CentralTransport::from_endpoint(url, Some(certificate_path))`.
    pub fn from_endpoint(
        origin: Url,
        ca_certificate_path: Option<&Path>,
    ) -> Result<Self, TransportError> {
        if !origin.username().is_empty() || origin.password().is_some() {
            return Err(TransportError::InvalidEndpoint(
                "credentials in endpoint; expected HTTPS origin only",
            ));
        }
        if origin.scheme() != "https" {
            return Err(TransportError::HttpsRequired(origin.to_string()));
        }
        if origin.host_str().is_none()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(TransportError::InvalidEndpoint(
                "endpoint path, query or fragment; expected HTTPS origin only",
            ));
        }
        let tls = TlsConnector::from(Arc::new(tls_client_config(ca_certificate_path)?));
        Ok(Self { origin, tls })
    }

    pub async fn connect(
        &self,
        control: &InvocationControl,
    ) -> Result<Http2Connection, TransportError> {
        if control.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if control.is_expired() {
            return Err(TransportError::DeadlineExceeded);
        }
        let host = self
            .origin
            .host()
            .ok_or(TransportError::MissingHost)?
            .to_string();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = self
            .origin
            .port_or_known_default()
            .ok_or(TransportError::MissingPort)?;
        let tcp = await_control(control, TcpStream::connect((host, port))).await??;
        let server_name = ServerName::try_from(host.to_owned())
            .map_err(|_| TransportError::InvalidServerName(host.to_owned()))?;
        let tls = await_control(control, self.tls.connect(server_name, tcp)).await??;
        let (sender, connection) = await_control(control, h2::client::handshake(tls)).await??;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Http2Connection {
            sender,
            authority: self.origin.authority().to_owned(),
        })
    }
}

pub struct Http2Connection {
    sender: SendRequest<Bytes>,
    authority: String,
}

impl Http2Connection {
    /// Opens a verified, authenticated request stream.  The response future is
    /// returned with the write stream so callers can keep a single H2
    /// invocation open for request/response frames.
    pub fn request(
        &mut self,
        path: &str,
        access_token: &str,
    ) -> Result<(ResponseFuture, SendStream<Bytes>), TransportError> {
        if !matches!(path, READINESS_PATH | INVOKE_PATH | CANCEL_PATH) {
            return Err(TransportError::UnsupportedRoute(path.to_owned()));
        }
        if access_token.trim().is_empty() {
            return Err(TransportError::MissingAccessToken);
        }
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::HOST, &self.authority)
            .header(header::CONTENT_TYPE, CONTENT_TYPE)
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(
                "lumvise-resource-protocol-major",
                PROTOCOL_MAJOR.to_string(),
            )
            .body(())
            .expect("fixed HTTP/2 request is valid");
        let (response, stream) = self.sender.send_request(request, false)?;
        Ok((response, stream))
    }
}

/// Domain-neutral authenticated client for the framed resource protocol.
/// Synchronous boundary consumed by owning resource adapters.  Implementors
/// are required to preserve the supplied control rather than minting a new
/// deadline/cancellation scope.
pub trait ResourceInvocationClient: Send + Sync {
    fn readiness(
        &self,
        request: &ReadinessRequestV1,
        control: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError>;
    fn invoke(
        &self,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError>;
    fn cancel(
        &self,
        envelope: &InvocationEnvelopeV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError>;
}

/// Blocking adapter for synchronous owning interfaces.  It has one private
/// current-thread runtime and serializes only transport driving; each request
/// still has its own H2 connection and its caller's control.
pub struct BlockingAuthenticatedFramedClient {
    client: AuthenticatedFramedClient,
    runtime: std::sync::Mutex<tokio::runtime::Runtime>,
}

impl BlockingAuthenticatedFramedClient {
    pub fn new(client: AuthenticatedFramedClient) -> Result<Self, TransportError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(TransportError::Runtime)?;
        Ok(Self {
            client,
            runtime: std::sync::Mutex::new(runtime),
        })
    }
}

impl ResourceInvocationClient for BlockingAuthenticatedFramedClient {
    fn readiness(
        &self,
        request: &ReadinessRequestV1,
        control: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError> {
        self.runtime
            .lock()
            .expect("central transport runtime mutex poisoned")
            .block_on(self.client.readiness(request, control))
    }

    fn invoke(
        &self,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
        self.runtime
            .lock()
            .expect("central transport runtime mutex poisoned")
            .block_on(self.client.invoke(envelopes, control))
    }

    fn cancel(
        &self,
        envelope: &InvocationEnvelopeV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError> {
        self.runtime
            .lock()
            .expect("central transport runtime mutex poisoned")
            .block_on(self.client.cancel(envelope, control))
    }
}

///
/// It deliberately accepts the already-acquired bearer token: OIDC token
/// acquisition remains in `auth`, while this type owns only verified H2 and
/// frame correctness.  No domain adapter may bypass it with a local URL.
pub struct AuthenticatedFramedClient {
    transport: Arc<Http2CentralTransport>,
    access_token: Arc<str>,
}

impl AuthenticatedFramedClient {
    pub fn new(transport: Arc<Http2CentralTransport>, access_token: impl Into<Arc<str>>) -> Self {
        Self {
            transport,
            access_token: access_token.into(),
        }
    }

    pub async fn readiness(
        &self,
        request: &ReadinessRequestV1,
        control: &InvocationControl,
    ) -> Result<ReadinessResponseV1, TransportError> {
        let response = self
            .exchange(READINESS_PATH, encode_frame(request), control)
            .await?;
        decode_frame(&response).map_err(TransportError::Frame)
    }

    pub async fn invoke(
        &self,
        envelopes: &[InvocationEnvelopeV1],
        control: &InvocationControl,
    ) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
        if envelopes.is_empty() {
            return Err(TransportError::EmptyInvocation);
        }
        let mut body = Vec::new();
        for envelope in envelopes {
            body.extend_from_slice(&encode_frame(envelope));
        }
        decode_envelopes(&self.exchange(INVOKE_PATH, body, control).await?)
    }

    pub async fn cancel(
        &self,
        envelope: &InvocationEnvelopeV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, TransportError> {
        let body = self
            .exchange(CANCEL_PATH, encode_frame(envelope), control)
            .await?;
        let envelopes = decode_envelopes(&body)?;
        let Some(envelope) = envelopes.into_iter().next() else {
            return Err(TransportError::EmptyResponse);
        };
        match envelope.payload {
            Some(crate::protocol::invocation_envelope_v1::Payload::Terminal(terminal)) => {
                Ok(terminal)
            }
            _ => Err(TransportError::UnexpectedResponse),
        }
    }

    async fn exchange(
        &self,
        path: &str,
        body: Vec<u8>,
        control: &InvocationControl,
    ) -> Result<Vec<u8>, TransportError> {
        if control.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if control.is_expired() {
            return Err(TransportError::DeadlineExceeded);
        }
        let mut connection = self.transport.connect(control).await?;
        let (response, mut send) = connection.request(path, &self.access_token)?;
        send_body(&mut send, body, control).await?;
        let response = await_control(control, response).await??;
        if response.status() != http::StatusCode::OK {
            return Err(TransportError::HttpStatus(response.status()));
        }
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some(CONTENT_TYPE)
        {
            return Err(TransportError::InvalidResponseContentType);
        }
        let mut receive = response.into_body();
        let mut output = Vec::new();
        loop {
            let Some(chunk) = await_control(control, receive.data()).await? else {
                break;
            };
            let chunk = chunk?;
            receive.flow_control().release_capacity(chunk.len())?;
            output.extend_from_slice(&chunk);
        }
        if output.is_empty() {
            return Err(TransportError::EmptyResponse);
        }
        Ok(output)
    }
}

async fn send_body(
    stream: &mut SendStream<Bytes>,
    body: Vec<u8>,
    control: &InvocationControl,
) -> Result<(), TransportError> {
    let body = Bytes::from(body);
    stream.reserve_capacity(body.len());
    let mut offset = 0;
    while offset < body.len() {
        let capacity = await_control(control, poll_fn(|cx| stream.poll_capacity(cx)))
            .await?
            .ok_or(TransportError::ClosedRequestStream)??;
        if capacity == 0 {
            continue;
        }
        let end = offset.saturating_add(capacity).min(body.len());
        let eos = end == body.len();
        stream.send_data(body.slice(offset..end), eos)?;
        offset = end;
    }
    if body.is_empty() {
        stream.send_data(Bytes::new(), true)?;
    }
    Ok(())
}

fn decode_envelopes(body: &[u8]) -> Result<Vec<InvocationEnvelopeV1>, TransportError> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        if body.len() - offset < 8 {
            return Err(TransportError::TruncatedEnvelopeFrame);
        }
        let declared =
            u64::from_be_bytes(body[offset..offset + 8].try_into().expect("eight bytes"));
        let length = usize::try_from(declared)
            .map_err(|_| TransportError::LengthNotRepresentable(declared))?;
        let end = offset
            .checked_add(8)
            .and_then(|start| start.checked_add(length))
            .ok_or(TransportError::LengthNotRepresentable(declared))?;
        if end > body.len() {
            return Err(TransportError::TruncatedEnvelopeFrame);
        }
        frames.push(decode_frame(&body[offset..end]).map_err(TransportError::Frame)?);
        offset = end;
    }
    if frames.is_empty() {
        return Err(TransportError::EmptyResponse);
    }
    Ok(frames)
}

fn tls_client_config(ca_certificate_path: Option<&Path>) -> Result<ClientConfig, TransportError> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_certificate_path {
        let file = std::fs::File::open(path)?;
        let mut reader = BufReader::new(file);
        let certificates: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
            .collect::<Result<_, _>>()
            .map_err(TransportError::InvalidCaCertificate)?;
        if certificates.is_empty() {
            return Err(TransportError::EmptyCaCertificate);
        }
        roots.add_parsable_certificates(certificates);
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

async fn await_control<T>(
    control: &InvocationControl,
    operation: impl Future<Output = T>,
) -> Result<T, TransportError> {
    if control.is_cancelled() {
        return Err(TransportError::Cancelled);
    }
    if control.is_expired() {
        return Err(TransportError::DeadlineExceeded);
    }
    let cancellation = control.cancellation_token();
    tokio::select! {
        _ = cancellation.cancelled() => Err(TransportError::Cancelled),
        result = tokio::time::timeout(control.remaining(), operation) => {
            result.map_err(|_| TransportError::DeadlineExceeded)
        }
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("invalid central server endpoint: {0}")]
    InvalidEndpoint(&'static str),
    #[error("central server URL must use HTTPS, got {0}")]
    HttpsRequired(String),
    #[error("central server URL has no hostname")]
    MissingHost,
    #[error("central server URL has no known port")]
    MissingPort,
    #[error("central server hostname is invalid for TLS: {0}")]
    InvalidServerName(String),
    #[error("central transport requires a bearer access token")]
    MissingAccessToken,
    #[error("shared invocation was cancelled")]
    Cancelled,
    #[error("shared invocation deadline expired")]
    DeadlineExceeded,
    #[error("unsupported resource route {0}")]
    UnsupportedRoute(String),
    #[error("resource request stream closed before its body was written")]
    ClosedRequestStream,
    #[error("resource server responded with HTTP status {0}")]
    HttpStatus(http::StatusCode),
    #[error("resource server response did not declare the framed protocol content type")]
    InvalidResponseContentType,
    #[error("resource response body was empty")]
    EmptyResponse,
    #[error("invoke requires at least one envelope")]
    EmptyInvocation,
    #[error("resource response did not contain the expected terminal envelope")]
    UnexpectedResponse,
    #[error("resource response ended in the middle of a framed envelope")]
    TruncatedEnvelopeFrame,
    #[error("resource frame length {0} cannot fit local usize")]
    LengthNotRepresentable(u64),
    #[error("resource frame failed: {0}")]
    Frame(#[from] crate::protocol::FrameError),
    #[error("central transport runtime failed to initialize: {0}")]
    Runtime(std::io::Error),
    #[error("cannot read CA certificate: {0}")]
    Io(#[from] std::io::Error),
    #[error("CA certificate PEM is invalid: {0}")]
    InvalidCaCertificate(std::io::Error),
    #[error("CA certificate PEM contains no certificate")]
    EmptyCaCertificate,
    #[error("HTTP/2 error: {0}")]
    H2(#[from] h2::Error),
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::protocol::{
        InvocationEnvelopeV1, TypedBinaryChunkV1, encode_frame, invocation_envelope_v1::Payload,
    };

    fn envelope(sequence: u64, bytes: Vec<u8>) -> InvocationEnvelopeV1 {
        InvocationEnvelopeV1 {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: crate::protocol::PROTOCOL_MINOR,
            request_id: "request".into(),
            sequence,
            payload: Some(Payload::BinaryChunk(TypedBinaryChunkV1 {
                type_name: "application/octet-stream".into(),
                bytes,
                metadata_json: None,
                final_chunk: sequence == 1,
            })),
        }
    }

    #[test]
    fn framed_response_keeps_every_binary_envelope() {
        let expected = vec![envelope(0, vec![0, 255]), envelope(1, vec![1, 2, 3])];
        let body = expected.iter().flat_map(encode_frame).collect::<Vec<_>>();
        assert_eq!(decode_envelopes(&body).unwrap(), expected);
    }

    #[test]
    fn central_transport_rejects_non_tls_url_even_when_constructed_directly() {
        let config = CentralServerConfig {
            url: Url::parse("http://resources.example").unwrap(),
            ca_certificate_path: None,
            oidc: crate::config::OidcClientConfig {
                issuer_url: Url::parse("https://issuer.example").unwrap(),
                client_id: "desktop".into(),
                audience: "resources".into(),
                scopes: vec!["lumvise.resources".into()],
                ca_certificate_path: None,
            },
        };
        assert!(matches!(
            Http2CentralTransport::from_config(&config),
            Err(TransportError::HttpsRequired(_))
        ));
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_transport_wait() {
        let control = InvocationControl::sixty_seconds();
        let canceller = control.child();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            canceller.cancel();
        });
        let result = await_control(&control, tokio::time::sleep(Duration::from_secs(1))).await;
        assert!(matches!(result, Err(TransportError::Cancelled)));
    }
}
