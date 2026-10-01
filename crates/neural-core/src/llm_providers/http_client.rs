use crate::error::{NeuralError, Result};
use crate::llm_providers::contract::{LlmHttpClient, LlmHttpRequest, LlmTextChunkSink};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::io::{ErrorKind, Read};
use std::ops::ControlFlow;
use std::time::Duration;

// A provider that never responds (or stalls mid-stream) must still surface
// as a bounded failure instead of hanging the caller's generation job
// forever. Bounds the whole request/response including a streamed body
// (`reqwest::blocking` has no per-read timeout) — generous enough for a
// large completion, not infinite.
const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

// Provider payloads can embed kilobytes of context; keep every error message
// bounded while preserving the actionable prefix.
const MAX_MESSAGE_CHARS: usize = 2000;

fn bounded_message(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_MESSAGE_CHARS) {
        None => text.to_string(),
        Some((byte_index, _)) => format!(
            "{}… ({} more bytes)",
            &text[..byte_index],
            text.len() - byte_index
        ),
    }
}

// reqwest hides the actionable reason (dns error, connection refused) in the
// error's `source()` chain; the outer Display alone is not enough to diagnose.
fn source_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = std::error::Error::source(cause);
    }
    message
}

pub struct ReqwestLlmHttpClient {
    client: Client,
}

impl ReqwestLlmHttpClient {
    /// Creates a reusable HTTP client for remote LLM providers.
    ///
    /// # Example
    ///
    /// ```
    /// let _client = lumvise_neural_core::llm_providers::http_client::ReqwestLlmHttpClient::new();
    /// ```
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for ReqwestLlmHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmHttpClient for ReqwestLlmHttpClient {
    fn get_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        let timeout = request_timeout(request)?;
        let response = self
            .client
            .get(&request.endpoint)
            .timeout(timeout)
            .headers(headers(request)?)
            .send()
            .map_err(|error| provider_error(&request.endpoint, error, timeout))?;
        parse_json_response(&request.endpoint, response, timeout)
    }

    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        let timeout = request_timeout(request)?;
        let response = self
            .client
            .post(&request.endpoint)
            .timeout(timeout)
            .headers(headers(request)?)
            .json(&request.payload)
            .send()
            .map_err(|error| provider_error(&request.endpoint, error, timeout))?;
        parse_json_response(&request.endpoint, response, timeout)
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>> {
        let timeout = request_timeout(request)?;
        let response = self
            .client
            .post(&request.endpoint)
            .timeout(timeout)
            .headers(headers(request)?)
            .json(&request.payload)
            .send()
            .map_err(|error| provider_error(&request.endpoint, error, timeout))?;
        parse_text_response(&request.endpoint, response, timeout)
    }

    fn stream_text_with_chunks(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut LlmTextChunkSink<'_>,
    ) -> Result<()> {
        self.stream_text_until(request, &mut |chunk| {
            on_chunk(chunk)?;
            Ok(ControlFlow::Continue(()))
        })
    }

    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    ) -> Result<()> {
        let timeout = request_timeout(request)?;
        let response = self
            .client
            .post(&request.endpoint)
            .timeout(timeout)
            .headers(headers(request)?)
            .json(&request.payload)
            .send()
            .map_err(|error| provider_error(&request.endpoint, error, timeout))?;
        stream_text_response(&request.endpoint, response, on_chunk, timeout)
    }
}

/// Per-request bound: the caller's remaining time, never above the client
/// ceiling. An exhausted deadline fails before any bytes are sent so the
/// provider worker lane is released immediately.
fn request_timeout(request: &LlmHttpRequest) -> Result<Duration> {
    match request.timeout {
        Some(remaining) if remaining.is_zero() => Err(NeuralError::ProcessTimeout {
            command: request.endpoint.clone(),
            timeout_ms: 0,
        }),
        Some(remaining) => Ok(remaining.min(PROVIDER_REQUEST_TIMEOUT)),
        None => Ok(PROVIDER_REQUEST_TIMEOUT),
    }
}

fn headers(request: &LlmHttpRequest) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    // Local OpenAI-compatible servers (Ollama, vLLM, LM Studio) reject or
    // ignore empty bearer tokens; omit the header entirely when no credential
    // is configured. Non-empty credentials keep the existing Bearer behavior.
    if !request.credential.trim().is_empty() {
        headers.insert(AUTHORIZATION, bearer_header(&request.credential)?);
    }
    for (name, value) in &request.headers {
        headers.insert(header_name(name)?, header_value(name, value)?);
    }
    Ok(headers)
}

fn bearer_header(credential: &str) -> Result<HeaderValue> {
    header_value("Authorization", &format!("Bearer {credential}"))
}

fn header_name(name: &str) -> Result<HeaderName> {
    HeaderName::from_bytes(name.as_bytes()).map_err(|error| NeuralError::ProviderFailed {
        provider_id: "http".to_string(),
        message: bounded_message(&format!("invalid header name `{name}`: {error}")),
    })
}

fn header_value(name: &str, value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value).map_err(|error| NeuralError::ProviderFailed {
        provider_id: "http".to_string(),
        message: bounded_message(&format!("invalid header value for `{name}`: {error}")),
    })
}

fn parse_json_response(
    endpoint: &str,
    response: reqwest::blocking::Response,
    timeout: Duration,
) -> Result<Value> {
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| provider_error(endpoint, error, timeout))?;
    ensure_success(endpoint, status.as_u16(), &body)?;
    serde_json::from_str(&body).map_err(|source| NeuralError::Json {
        value: body,
        expected: "LLM provider JSON response".to_string(),
        source,
    })
}

fn parse_text_response(
    endpoint: &str,
    response: reqwest::blocking::Response,
    timeout: Duration,
) -> Result<Vec<String>> {
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| provider_error(endpoint, error, timeout))?;
    ensure_success(endpoint, status.as_u16(), &body)?;
    Ok(vec![body])
}

fn stream_text_response(
    endpoint: &str,
    mut response: reqwest::blocking::Response,
    on_chunk: &mut dyn FnMut(String) -> Result<ControlFlow<()>>,
    timeout: Duration,
) -> Result<()> {
    let status = response.status();
    if !(200..300).contains(&status.as_u16()) {
        // A non-2xx streaming response carries the provider's error JSON in
        // the body; read a bounded prefix so the failure explains itself
        // instead of logging `HTTP 404: `.
        let mut limited = response.take(64 * 1024);
        let mut body = Vec::new();
        limited
            .read_to_end(&mut body)
            .map_err(|error| stream_error(endpoint, error, timeout))?;
        ensure_success(endpoint, status.as_u16(), &String::from_utf8_lossy(&body))?;
        return Ok(());
    }
    let mut buffer = [0_u8; 8192];
    let mut pending = Vec::new();
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| stream_error(endpoint, error, timeout))?;
        if read == 0 {
            return if pending.is_empty() {
                Ok(())
            } else {
                Err(invalid_stream_utf8(&pending))
            };
        }
        pending.extend_from_slice(&buffer[..read]);
        let text = take_stream_text(&mut pending)?;
        if !text.is_empty() && on_chunk(text)?.is_break() {
            return Ok(());
        }
    }
}

fn ensure_success(endpoint: &str, status: u16, body: &str) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(NeuralError::ProviderFailed {
        provider_id: endpoint.to_string(),
        message: format!("HTTP {status}: {}", bounded_message(body)),
    })
}

fn provider_error(endpoint: &str, error: reqwest::Error, timeout: Duration) -> NeuralError {
    // reqwest reports the per-request (and client-wide) timeout as a
    // generic error body; classify it so executors see DeadlineExceeded.
    if error.is_timeout() {
        return NeuralError::ProcessTimeout {
            command: endpoint.to_string(),
            timeout_ms: timeout_ms(timeout),
        };
    }
    // Transport-level failures (connect/send/body — no HTTP status at all) map
    // to the Io form so the executor classifies them as retryable
    // TransportReset instead of fatal ProviderRejected.
    if error.is_connect() || error.is_request() || error.is_body() {
        return NeuralError::Io {
            value: endpoint.to_string(),
            expected: "reachable LLM provider endpoint".to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                bounded_message(&source_chain(&error)),
            ),
        };
    }
    NeuralError::ProviderFailed {
        provider_id: endpoint.to_string(),
        message: bounded_message(&error.to_string()),
    }
}

fn stream_error(endpoint: &str, error: std::io::Error, timeout: Duration) -> NeuralError {
    // Response::read wraps reqwest timeouts in ErrorKind::Other. Preserve
    // their typed deadline instead of reporting a non-retryable rejection.
    if error
        .get_ref()
        .and_then(|source| source.downcast_ref::<reqwest::Error>())
        .is_some_and(reqwest::Error::is_timeout)
    {
        return NeuralError::ProcessTimeout {
            command: endpoint.to_string(),
            timeout_ms: timeout_ms(timeout),
        };
    }
    let message = bounded_message(&source_chain(&error));
    match error.kind() {
        ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof => {
            NeuralError::Io {
                value: endpoint.to_string(),
                expected: "reachable LLM provider endpoint".to_string(),
                source: std::io::Error::new(ErrorKind::ConnectionReset, message),
            }
        }
        ErrorKind::TimedOut => NeuralError::ProcessTimeout {
            command: endpoint.to_string(),
            timeout_ms: timeout_ms(timeout),
        },
        _ => NeuralError::ProviderFailed {
            provider_id: endpoint.to_string(),
            message,
        },
    }
}

fn timeout_ms(timeout: Duration) -> u64 {
    let fractional_millisecond = u64::from(!timeout.subsec_nanos().is_multiple_of(1_000_000));
    (timeout.as_millis() as u64 + fractional_millisecond).max(1)
}

// A network read can split a code point inside a tool argument or spoken text.
fn take_stream_text(pending: &mut Vec<u8>) -> Result<String> {
    let valid = match std::str::from_utf8(pending) {
        Ok(text) => text.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return Err(invalid_stream_utf8(pending)),
    };
    let text = String::from_utf8(pending.drain(..valid).collect())
        .map_err(|error| invalid_stream_utf8(error.as_bytes()))?;
    Ok(text)
}

fn invalid_stream_utf8(bytes: &[u8]) -> NeuralError {
    NeuralError::MalformedPayload {
        value: bounded_message(&format!("{bytes:?}")),
        expected: "complete UTF-8 provider stream".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_credential_omits_authorization_header() {
        let headers = headers(&LlmHttpRequest {
            timeout: None,
            endpoint: "http://localhost:11434/v1/chat/completions".to_string(),
            credential: String::new(),
            headers: Default::default(),
            payload: serde_json::json!({}),
        })
        .unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(headers.contains_key(CONTENT_TYPE));
    }

    #[test]
    fn non_empty_credential_keeps_bearer_authorization() {
        let headers = headers(&LlmHttpRequest {
            timeout: None,
            endpoint: "http://localhost:11434/v1/chat/completions".to_string(),
            credential: "test-token".to_string(),
            headers: Default::default(),
            payload: serde_json::json!({}),
        })
        .unwrap();
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-token")
        );
    }

    fn streaming_request(endpoint: String) -> LlmHttpRequest {
        LlmHttpRequest {
            timeout: None,
            endpoint,
            credential: String::new(),
            headers: Default::default(),
            payload: serde_json::json!({"model": "test", "stream": true}),
        }
    }

    // One-shot local HTTP server: answers the first request with `response`
    // bytes, then exits.
    fn serve_once(response: String) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 4096];
            let mut request = Vec::new();
            loop {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            use std::io::Write as _;
            stream.write_all(response.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn bounded_message_preserves_short_text() {
        assert_eq!(bounded_message("  hello  "), "hello");
    }

    #[test]
    fn bounded_message_truncates_on_char_boundary() {
        // 1500 two-byte chars + 600 four-byte chars: the 2000th char sits inside
        // the four-byte run, so a byte-index cut would split a code point.
        let mut text = "é".repeat(1500);
        text.push_str(&"🎉".repeat(600));
        let bounded = bounded_message(&text);
        let (prefix, suffix) = bounded.split_once('…').expect("truncation suffix");
        assert_eq!(prefix.chars().count(), 2000);
        assert!(prefix.chars().all(|c| c == 'é' || c == '🎉'));
        // 5400 total bytes − 5000 bytes for the kept 2000 chars = 400 more.
        assert!(suffix.contains("(400 more bytes)"), "suffix: {suffix}");
    }

    #[test]
    fn ensure_success_includes_response_body() {
        let error = ensure_success(
            "http://127.0.0.1:9/v1/chat/completions",
            404,
            "{\"error\":\"model ignored by guardrail\"}\n",
        )
        .unwrap_err();
        match error {
            NeuralError::ProviderFailed { message, .. } => {
                assert!(message.contains("HTTP 404"), "message: {message}");
                assert!(
                    message.contains("model ignored by guardrail"),
                    "message: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ensure_success_bounds_large_payload() {
        let body = "x".repeat(10 * 1024);
        let error = ensure_success("http://127.0.0.1:9/v1", 500, &body).unwrap_err();
        let NeuralError::ProviderFailed { message, .. } = error else {
            panic!("unexpected error kind");
        };
        assert!(
            message.chars().count() < 2000 + 40,
            "len: {}",
            message.len()
        );
        assert!(message.contains("more bytes"), "message: {message}");
    }

    #[test]
    fn stream_non_2xx_error_includes_response_body() {
        let body = r#"{"error":{"message":"model ignored by guardrail","code":404}}"#;
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let port = serve_once(response);
        let client = ReqwestLlmHttpClient::new();
        let error = client
            .stream_text_with_chunks(
                &streaming_request(format!("http://127.0.0.1:{port}/v1/chat/completions")),
                &mut |_| Ok(()),
            )
            .unwrap_err();
        let NeuralError::ProviderFailed { message, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(message.contains("HTTP 404"), "message: {message}");
        assert!(
            message.contains("model ignored by guardrail"),
            "message: {message}"
        );
    }

    #[test]
    fn connect_failure_maps_to_retryable_io_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let client = ReqwestLlmHttpClient::new();
        let error = client
            .post_json(&streaming_request(format!(
                "http://127.0.0.1:{port}/v1/chat/completions"
            )))
            .unwrap_err();
        let NeuralError::Io {
            value,
            expected,
            source,
        } = error
        else {
            panic!("unexpected error: {error:?}");
        };
        // ErrorKind::ConnectionReset maps to retryable TransportReset in the
        // executor (`from_neural_error`); ProviderFailed would be fatal.
        assert_eq!(source.kind(), std::io::ErrorKind::ConnectionReset);
        assert_eq!(expected, "reachable LLM provider endpoint");
        assert!(value.contains(&port.to_string()));
        let message = source.to_string();
        assert!(
            message.contains("error sending request for url"),
            "message: {message}"
        );
        // The `source()` chain must survive: the outer reqwest Display alone
        // hides the actual cause (OS connection refused).
        assert!(message.contains("Connection refused"), "message: {message}");
    }

    #[test]
    fn stream_reset_maps_to_retryable_io_error() {
        let error = stream_error(
            "http://127.0.0.1:9/v1",
            std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset by peer",
            ),
            Duration::from_millis(250),
        );
        let NeuralError::Io { source, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::ConnectionReset);
        assert!(source.to_string().contains("connection reset by peer"));
    }

    #[test]
    fn stream_timeout_maps_to_process_timeout() {
        let error = stream_error(
            "http://127.0.0.1:9/v1",
            std::io::Error::new(std::io::ErrorKind::TimedOut, "read timed out"),
            Duration::from_millis(250),
        );
        assert!(matches!(
            error,
            NeuralError::ProcessTimeout {
                timeout_ms: 250,
                ..
            }
        ));
    }

    #[test]
    fn timeout_ms_rounds_positive_fraction_up() {
        assert_eq!(timeout_ms(Duration::from_micros(1_500)), 2);
    }

    struct StalledSseServer {
        endpoint: String,
        release: std::sync::mpsc::Sender<()>,
        worker: std::thread::JoinHandle<()>,
    }

    impl StalledSseServer {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                use std::io::Write as _;
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                assert!(socket.read(&mut request).unwrap() > 0);
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 10000\r\n\r\n: heartbeat\n\n").unwrap();
                let _ = wait.recv_timeout(Duration::from_secs(5));
            });
            Self {
                endpoint: format!("http://{address}/v1/chat/completions"),
                release,
                worker,
            }
        }
    }

    #[test]
    fn actual_stream_body_timeout_maps_to_deadline_after_receiving_a_chunk() {
        let server = StalledSseServer::start();
        let mut request = streaming_request(server.endpoint);
        request.timeout = Some(Duration::from_millis(250));
        let mut received = String::new();
        let result = ReqwestLlmHttpClient::new().stream_text_with_chunks(&request, &mut |chunk| {
            received.push_str(&chunk);
            Ok(())
        });
        let _ = server.release.send(());
        server.worker.join().unwrap();
        assert_eq!(received, ": heartbeat\n\n");
        assert!(
            matches!(
                result,
                Err(NeuralError::ProcessTimeout {
                    timeout_ms: 250,
                    ..
                })
            ),
            "expected deadline failure after streamed headers, got {result:?}"
        );
    }

    #[test]
    fn actual_json_body_timeout_reports_effective_request_duration() {
        let server = StalledSseServer::start();
        let mut request = streaming_request(server.endpoint);
        request.timeout = Some(Duration::from_millis(250));
        let result = ReqwestLlmHttpClient::new().post_json(&request);
        let _ = server.release.send(());
        server.worker.join().unwrap();
        assert!(
            matches!(
                result,
                Err(NeuralError::ProcessTimeout {
                    timeout_ms: 250,
                    ..
                })
            ),
            "expected 250ms response-body deadline, got {result:?}"
        );
    }
}
