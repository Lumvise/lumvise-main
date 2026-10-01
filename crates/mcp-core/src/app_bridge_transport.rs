use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::Value;

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Classifies why an app bridge request failed so the caller can decide
/// whether rediscovering another app endpoint could help.
///
/// Transport failures (connection refused, timeout, malformed/no response) may
/// be recoverable by retrying against a different discovered endpoint. A
/// definitive HTTP status (e.g. 4xx/5xx) is a business response from the app
/// and must be surfaced to the caller without rediscovery or rerouting.
#[derive(Clone, Debug)]
pub enum AppBridgeRequestError {
    /// The request never produced a usable response (connection, IO, malformed).
    Transport(String),
    /// The app returned a definitive non-2xx HTTP response.
    HttpStatus { status: String, body: String },
}

impl std::error::Error for AppBridgeRequestError {}

impl std::fmt::Display for AppBridgeRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Transport(message) => formatter.write_str(message),
            Self::HttpStatus { status, body } => write!(
                formatter,
                "app bridge request failed with {status}; expected 2xx response: {body}"
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
/// Stateless HTTP/1.1 JSON transport for the app bridge.
pub struct AppBridgeHttpTransport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// HTTP methods supported by the app bridge wire protocol.
pub enum AppBridgeHttpMethod {
    /// Read a bridge resource without a request body.
    Get,
    /// Send a JSON body to a bridge operation.
    Post,
}

impl AppBridgeHttpTransport {
    /// Creates a stateless bridge transport.
    pub fn new() -> Self {
        Self
    }

    /// Sends a JSON POST and decodes its JSON response.
    pub fn post_json(
        &self,
        base_url: &str,
        path: &str,
        body: &Value,
    ) -> Result<Value, AppBridgeRequestError> {
        self.request_json(base_url, AppBridgeHttpMethod::Post, path, body)
    }

    /// Sends one supported HTTP request and decodes its JSON response.
    pub fn request_json(
        &self,
        base_url: &str,
        method: AppBridgeHttpMethod,
        path: &str,
        body: &Value,
    ) -> Result<Value, AppBridgeRequestError> {
        let response = self.request(base_url, method, path, body)?;
        serde_json::from_slice(&response).map_err(|error| {
            AppBridgeRequestError::Transport(format!(
                "invalid app bridge JSON {:?}; expected JSON: {error}",
                String::from_utf8_lossy(&response)
            ))
        })
    }

    /// Sends a versioned Protobuf POST and returns its opaque binary response.
    pub fn post_protobuf(
        &self,
        base_url: &str,
        path: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, AppBridgeRequestError> {
        self.request_bytes(
            base_url,
            AppBridgeHttpMethod::Post,
            path,
            "application/protobuf",
            body,
            HTTP_TIMEOUT,
        )
    }

    /// Sends Protobuf using the remaining absolute invocation deadline.
    pub fn post_protobuf_with_timeout(
        &self,
        base_url: &str,
        path: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, AppBridgeRequestError> {
        self.request_bytes(
            base_url,
            AppBridgeHttpMethod::Post,
            path,
            "application/protobuf",
            body,
            timeout,
        )
    }

    fn request(
        &self,
        base_url: &str,
        method: AppBridgeHttpMethod,
        path: &str,
        body: &Value,
    ) -> Result<Vec<u8>, AppBridgeRequestError> {
        let encoded = if method.includes_body() {
            body.to_string().into_bytes()
        } else {
            Vec::new()
        };
        self.request_bytes(
            base_url,
            method,
            path,
            "application/json",
            &encoded,
            HTTP_TIMEOUT,
        )
    }

    fn request_bytes(
        &self,
        base_url: &str,
        method: AppBridgeHttpMethod,
        path: &str,
        content_type: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, AppBridgeRequestError> {
        let address = http_address(base_url).map_err(AppBridgeRequestError::Transport)?;
        let payload = http_bytes(method, path, &address, content_type, body);
        let mut stream = TcpStream::connect(&address).map_err(|error| {
            AppBridgeRequestError::Transport(format!(
                "failed to connect to app bridge {address:?}; expected running app: {error}"
            ))
        })?;
        configure_stream(&stream, timeout);
        stream.write_all(&payload).map_err(|error| {
            AppBridgeRequestError::Transport(format!(
                "failed to write app bridge request to {address:?}: {error}"
            ))
        })?;
        read_http_response(stream)
    }
}

impl AppBridgeHttpMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }

    fn includes_body(self) -> bool {
        matches!(self, Self::Post)
    }
}

fn read_http_response(mut stream: TcpStream) -> Result<Vec<u8>, AppBridgeRequestError> {
    let mut reader = BufReader::new(&mut stream);
    let status = read_response_line(&mut reader)?;
    let content_length = read_response_content_length(&mut reader)?;
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).map_err(|error| {
        AppBridgeRequestError::Transport(format!(
            "failed to read app bridge response body: {error}"
        ))
    })?;
    response_body(status, body)
}

fn read_response_line(
    reader: &mut BufReader<&mut TcpStream>,
) -> Result<String, AppBridgeRequestError> {
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|error| {
        AppBridgeRequestError::Transport(format!(
            "failed to read app bridge response status: {error}"
        ))
    })?;
    Ok(line.trim_end().to_string())
}

fn read_response_content_length(
    reader: &mut BufReader<&mut TcpStream>,
) -> Result<usize, AppBridgeRequestError> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|error| {
            AppBridgeRequestError::Transport(format!(
                "failed to read app bridge response header: {error}"
            ))
        })?;
        let line = line.trim_end();
        if line.is_empty() {
            return content_length.ok_or_else(|| {
                AppBridgeRequestError::Transport(
                    "missing app bridge Content-Length header".to_string(),
                )
            });
        }
        content_length = response_content_length(line).or(content_length);
    }
}

fn response_content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if !name.eq_ignore_ascii_case("content-length") {
        return None;
    }
    value.trim().parse().ok()
}

fn response_body(status: String, body: Vec<u8>) -> Result<Vec<u8>, AppBridgeRequestError> {
    if status.contains(" 2") {
        return Ok(body);
    }
    Err(AppBridgeRequestError::HttpStatus {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn configure_stream(stream: &TcpStream, timeout: Duration) {
    let timeout = timeout.max(Duration::from_millis(1));
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
}

fn http_bytes(
    method: AppBridgeHttpMethod,
    path: &str,
    address: &str,
    content_type: &str,
    body: &[u8],
) -> Vec<u8> {
    let body = if method.includes_body() { body } else { &[] };
    let mut payload = format!(
        "{} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        method.as_str(), body.len()
    ).into_bytes();
    payload.extend_from_slice(body);
    payload
}

fn http_address(base_url: &str) -> Result<String, String> {
    base_url
        .strip_prefix("http://")
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("invalid app bridge URL {base_url:?}; expected http://host:port"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn http_payload_omits_body_for_get_requests() {
        let payload = http_bytes(
            AppBridgeHttpMethod::Get,
            "/health",
            "127.0.0.1:1",
            "application/json",
            br#"{"a":1}"#,
        );

        assert!(payload.ends_with(b"Content-Length: 0\r\n\r\n"));
    }

    #[test]
    fn http_address_rejects_non_http_urls() {
        let error = http_address("ws://127.0.0.1:1").unwrap_err();

        assert!(error.contains("expected http://host:port"));
    }

    #[test]
    fn transport_reads_content_length_without_waiting_for_socket_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"status":"ok"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            thread::sleep(Duration::from_millis(200));
        });

        let response = AppBridgeHttpTransport::new()
            .request_json(
                &format!("http://{address}"),
                AppBridgeHttpMethod::Get,
                "/api/mcp/plugins/surface",
                &Value::Null,
            )
            .unwrap();

        assert_eq!(response, json!({ "status": "ok" }));
        server.join().unwrap();
    }

    #[test]
    fn transport_allows_slow_app_plugin_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request).unwrap();
            thread::sleep(Duration::from_millis(2_200));
            let body = r#"{"status":"ok"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let response = AppBridgeHttpTransport::new()
            .request_json(
                &format!("http://{address}"),
                AppBridgeHttpMethod::Post,
                "/api/mcp/plugins/invoke",
                &json!({ "plugin_id": "builtin.knowledge" }),
            )
            .unwrap();

        assert_eq!(response, json!({ "status": "ok" }));
        server.join().unwrap();
    }

    #[test]
    fn classifies_non_2xx_response_as_definitive_http_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request);
            let body = r#"{"error":"session not found"}"#;
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let error = AppBridgeHttpTransport::new()
            .request_json(
                &format!("http://{address}"),
                AppBridgeHttpMethod::Get,
                "/api/mcp/plugins/surface",
                &Value::Null,
            )
            .unwrap_err();

        match error {
            AppBridgeRequestError::HttpStatus { status, body } => {
                assert!(status.contains("400"), "status was {status:?}");
                assert!(body.contains("session not found"), "body was {body:?}");
            }
            other => panic!("expected HttpStatus, got {other:?}"),
        }
        server.join().unwrap();
    }

    #[test]
    fn classifies_connection_refused_as_transport_failure() {
        // Bind to an ephemeral port then drop the listener so connects fail.
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();

        let error = AppBridgeHttpTransport::new()
            .request_json(
                &format!("http://{address}"),
                AppBridgeHttpMethod::Get,
                "/api/mcp/plugins/surface",
                &Value::Null,
            )
            .unwrap_err();

        assert!(
            matches!(error, AppBridgeRequestError::Transport(_)),
            "expected Transport, got {error:?}"
        );
    }
}
