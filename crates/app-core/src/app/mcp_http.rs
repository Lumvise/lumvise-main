use super::plugin_background_driver;
use crate::{AppCore, AppCoreError, plugin::compiled_surfaces::CompiledSseRoute};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::env;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener as AsyncTcpListener, TcpStream as AsyncTcpStream};
use tokio::runtime::Builder;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

const DEFAULT_HTTP_MAX_CONCURRENCY: usize = 128;
const DEFAULT_HTTP_QUEUE_DEPTH: usize = 256;

#[cfg(test)]
static GENERIC_HTTP_ROUTER_INVOKE_V1_SPAWNS: AtomicUsize = AtomicUsize::new(0);

pub struct ScopedMcpHttpServer {
    app: Arc<AppCore>,
    base_url: String,
    shutdown: Arc<AtomicBool>,
    shutdown_signal: watch::Sender<bool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    plugin_background_handle: Mutex<Option<JoinHandle<()>>>,
}

pub(crate) struct HttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: BTreeMap<String, String>,
    pub(crate) authorization: Option<String>,
    pub(crate) body: Vec<u8>,
}

impl ScopedMcpHttpServer {
    /// Starts the local scoped MCP HTTP adapter and stores its base URL in App Core.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let app = std::sync::Arc::new(lumvise_app_core::AppCore::in_memory().unwrap());
    /// let server = lumvise_app_core::ScopedMcpHttpServer::spawn(app).unwrap();
    /// assert!(server.base_url().starts_with("http://127.0.0.1:"));
    /// ```
    pub fn spawn(app: Arc<AppCore>) -> crate::Result<Self> {
        let listener = bind_listener()?;
        let address = listener.local_addr().map_err(http_error)?;
        let base_url = format!("http://{address}");
        app.set_scoped_mcp_base_url(Some(base_url.clone()))?;
        Ok(Self::from_listener(app, listener, base_url))
    }

    /// Returns the base URL configured for assistant LLM MCP access.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn from_listener(app: Arc<AppCore>, listener: TcpListener, base_url: String) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let (shutdown_signal, shutdown_receiver) = watch::channel(false);
        let handle = spawn_server_thread(
            Arc::clone(&app),
            listener,
            Arc::clone(&shutdown),
            shutdown_receiver,
        );
        let plugin_background_handle =
            plugin_background_driver::spawn(Arc::clone(&app), Arc::clone(&shutdown));
        Self {
            app,
            base_url,
            shutdown,
            shutdown_signal,
            handle: Mutex::new(Some(handle)),
            plugin_background_handle: Mutex::new(Some(plugin_background_handle)),
        }
    }
}

impl Drop for ScopedMcpHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.shutdown_signal.send(true);
        let _ = self.app.notify_background_registration_change();
        let _ = self.app.clear_scoped_mcp_base_url_if(&self.base_url);
        if let Ok(mut handle) = self.handle.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
        if let Ok(mut handle) = self.plugin_background_handle.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
        // T3.4: final on-shutdown WAL checkpoint so freed graph space is reclaimed
        // Best-effort selected semantic maintenance; failure does not alter
        // shutdown semantics.
        let _ = self.app.semantic.execute(
            lumvise_db_core::SemanticOperation::Maintenance,
            &lumvise_resource_routing::InvocationControl::sixty_seconds(),
        );
    }
}

fn bind_listener() -> crate::Result<TcpListener> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(http_error)?;
    listener.set_nonblocking(true).map_err(http_error)?;
    Ok(listener)
}

fn spawn_server_thread(
    app: Arc<AppCore>,
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    shutdown_receiver: watch::Receiver<bool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let runtime = match Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(
                    target: "lumvise-app-core::mcp-http",
                    event = "async_runtime_start_failed",
                    error = %error,
                    "cannot start scoped MCP HTTP runtime"
                );
                return;
            }
        };
        runtime.block_on(async move {
            let listener = match AsyncTcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::error!(
                        target: "lumvise-app-core::mcp-http",
                        event = "async_listener_adoption_failed",
                        error = %error,
                        "cannot adopt scoped MCP listener into tokio"
                    );
                    return;
                }
            };
            run_server_loop(app, listener, shutdown, shutdown_receiver).await;
        });
    })
}

/// Bounds normal handler work by configuration instead of fixed reader/control/invocation
/// worker counts. `outstanding` includes active and queued requests, so input admission stays
/// bounded even though waiting happens asynchronously.
struct HttpAdmission {
    permits: Arc<Semaphore>,
    max_outstanding: usize,
    outstanding: AtomicUsize,
    enforce: bool,
}

impl HttpAdmission {
    fn from_environment() -> Arc<Self> {
        let max_concurrency =
            env_limit("LUMVISE_HTTP_MAX_CONCURRENCY", DEFAULT_HTTP_MAX_CONCURRENCY);
        let queue_depth = env_limit("LUMVISE_HTTP_QUEUE_DEPTH", DEFAULT_HTTP_QUEUE_DEPTH);
        Arc::new(Self {
            permits: Arc::new(Semaphore::new(max_concurrency)),
            max_outstanding: max_concurrency.saturating_add(queue_depth),
            outstanding: AtomicUsize::new(0),
            enforce: env::var("LUMVISE_HTTP_NO_ENFORCE").ok().as_deref() != Some("1"),
        })
    }

    async fn acquire(self: &Arc<Self>) -> Result<HttpAdmissionPermit, ()> {
        if !self.enforce {
            return Ok(HttpAdmissionPermit::unlimited());
        }
        if self
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.max_outstanding).then_some(current + 1)
            })
            .is_err()
        {
            return Err(());
        }
        let permit = self.permits.clone().acquire_owned().await.map_err(|_| ())?;
        Ok(HttpAdmissionPermit {
            permit: Some(permit),
            admission: Some(Arc::clone(self)),
        })
    }
}

struct HttpAdmissionPermit {
    permit: Option<OwnedSemaphorePermit>,
    admission: Option<Arc<HttpAdmission>>,
}

impl HttpAdmissionPermit {
    fn unlimited() -> Self {
        Self {
            permit: None,
            admission: None,
        }
    }
}

impl Drop for HttpAdmissionPermit {
    fn drop(&mut self) {
        self.permit.take();
        if let Some(admission) = self.admission.take() {
            admission.outstanding.fetch_sub(1, Ordering::Release);
        }
    }
}

fn env_limit(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

async fn run_server_loop(
    app: Arc<AppCore>,
    listener: AsyncTcpListener,
    shutdown: Arc<AtomicBool>,
    mut shutdown_receiver: watch::Receiver<bool>,
) {
    let admission = HttpAdmission::from_environment();
    loop {
        tokio::select! {
            changed = shutdown_receiver.changed() => {
                if changed.is_err() || *shutdown_receiver.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let app = Arc::clone(&app);
                let admission = Arc::clone(&admission);
                let shutdown = Arc::clone(&shutdown);
                let shutdown_receiver = shutdown_receiver.clone();
                tokio::spawn(async move {
                    serve_connection(app, stream, admission, shutdown, shutdown_receiver).await;
                });
            }
        }
    }
}

async fn serve_connection(
    app: Arc<AppCore>,
    mut stream: AsyncTcpStream,
    admission: Arc<HttpAdmission>,
    shutdown: Arc<AtomicBool>,
    shutdown_receiver: watch::Receiver<bool>,
) {
    let request = match read_http_request(&mut stream).await {
        Ok(request) => request,
        Err(error) => {
            let _ = write_async_error(&mut stream, "400 Bad Request", &error).await;
            return;
        }
    };

    if super::http_router::is_storage_change_stream_request(&request) {
        super::storage_change_stream::serve(app, stream, request, shutdown_receiver).await;
        return;
    }

    // Compiled SSE writes directly to a std::net::TcpStream today. It must keep that
    // established process-boundary writer until its separate async-stream migration.
    if request.path.contains("/events") {
        serve_streaming_connection(app, stream, request, shutdown).await;
        return;
    }

    let permit = match admission.acquire().await {
        Ok(permit) => permit,
        Err(()) => {
            let _ = write_async_error(
                &mut stream,
                "503 Service Unavailable",
                "local App Bridge connection admission is full",
            )
            .await;
            return;
        }
    };

    let method = request.method.clone();
    let route = crate::observability::route_metric_label(&request.path);
    if request.method == "POST" && request.path == "/api/mcp/plugins/invoke-v1" {
        serve_controlled_invocation(app, stream, request, permit, method, route).await;
        return;
    }

    let started = Instant::now();
    #[cfg(test)]
    if request.path == "/api/mcp/plugins/invoke-v1" {
        GENERIC_HTTP_ROUTER_INVOKE_V1_SPAWNS.fetch_add(1, Ordering::AcqRel);
    }
    let response = match tokio::task::spawn_blocking({
        let app = Arc::clone(&app);
        move || super::http_router::route_http_request(&app, request)
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error_response("500 Internal Server Error", error.to_string()),
    };
    crate::observability::record_http_request(&method, &route, &response.status, started.elapsed());
    let _ = write_async_http_response(&mut stream, response).await;
}

async fn serve_controlled_invocation(
    app: Arc<AppCore>,
    stream: AsyncTcpStream,
    request: HttpRequest,
    _permit: HttpAdmissionPermit,
    method: String,
    route: String,
) {
    let started = Instant::now();
    let (request_id, handle) =
        match crate::plugin::mcp_http_bridge::start_controlled_invoke(&app, &request) {
            Ok(started) => started,
            Err(response) => {
                crate::observability::record_http_request(
                    &method,
                    &route,
                    &response.status,
                    started.elapsed(),
                );
                let mut stream = stream;
                let _ = write_async_http_response(&mut stream, response).await;
                return;
            }
        };
    let (read_half, mut write_half) = stream.into_split();
    tokio::pin!(handle);
    tokio::select! {
        biased;
        _ = wait_for_client_eof(read_half) => {
            handle.cancel();
            let _ = handle.await;
        }
        result = &mut handle => {
            let response = crate::plugin::mcp_http_bridge::controlled_invoke_result(request_id, result);
            crate::observability::record_http_request(
                &method,
                &route,
                &response.status,
                started.elapsed(),
            );
            let _ = write_async_http_response(&mut write_half, response).await;
        }
    }
}

async fn wait_for_client_eof(mut read_half: tokio::net::tcp::OwnedReadHalf) {
    let mut buffer = [0_u8; 256];
    loop {
        match read_half.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn serve_streaming_connection(
    app: Arc<AppCore>,
    stream: AsyncTcpStream,
    request: HttpRequest,
    shutdown: Arc<AtomicBool>,
) {
    let Ok(mut stream) = stream.into_std() else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        let method = request.method.clone();
        let route = crate::observability::route_metric_label(&request.path);
        let started = Instant::now();
        let response = super::http_router::route_http_request(&app, request);
        crate::observability::record_http_request(
            &method,
            &route,
            &response.status,
            started.elapsed(),
        );
        let _ = write_http_response(&mut stream, &app, response, &shutdown);
    })
    .await;
}

async fn read_http_request(
    stream: &mut AsyncTcpStream,
) -> std::result::Result<HttpRequest, String> {
    let mut reader = BufReader::new(stream);
    let request_line = read_header_line(&mut reader).await?;
    let (method, path, query) = parse_request_line(&request_line)?;
    let (content_length, authorization) = read_headers(&mut reader).await?;
    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    Ok(HttpRequest {
        method,
        path,
        query,
        authorization,
        body,
    })
}

async fn read_header_line(
    reader: &mut BufReader<&mut AsyncTcpStream>,
) -> std::result::Result<String, String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|error| error.to_string())?;
    Ok(line.trim_end().to_string())
}

async fn read_headers(
    reader: &mut BufReader<&mut AsyncTcpStream>,
) -> std::result::Result<(usize, Option<String>), String> {
    let mut content_length = 0;
    let mut authorization = None;
    loop {
        let line = read_header_line(reader).await?;
        if line.is_empty() {
            return Ok((content_length, authorization));
        }
        if let Some((name, value)) = parse_http_header(&line) {
            if name == "content-length" {
                content_length = value.parse().unwrap_or(content_length);
            } else if name == "authorization" {
                authorization = Some(value);
            }
        }
    }
}

fn parse_http_header(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once(':')?;
    Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
}

async fn write_async_error(
    stream: &mut AsyncTcpStream,
    status: &str,
    message: &str,
) -> std::io::Result<()> {
    let response = error_response(status, message);
    let (_, mut write_half) = stream.split();
    write_async_http_response(&mut write_half, response).await
}

pub(crate) async fn write_async_http_response(
    stream: &mut (impl AsyncWrite + Unpin),
    response: HttpResponse,
) -> std::io::Result<()> {
    let HttpResponse {
        status,
        content_type,
        body,
        headers,
    } = response;
    let HttpResponseBody::Buffered(body) = body else {
        return Err(std::io::Error::other(
            "streaming response routed through async buffered writer",
        ));
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await
}

fn parse_request_line(
    line: &str,
) -> std::result::Result<(String, String, BTreeMap<String, String>), String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 {
        return Err(format!(
            "invalid request line {line:?}; expected METHOD PATH HTTP"
        ));
    }
    let (path, query) = split_path_and_query(parts[1]);
    Ok((parts[0].to_string(), path.to_string(), query_params(query)?))
}

fn split_path_and_query(path: &str) -> (&str, Option<&str>) {
    match path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path, None),
    }
}

fn query_params(query: Option<&str>) -> std::result::Result<BTreeMap<String, String>, String> {
    let mut params = BTreeMap::new();
    for pair in query
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(decode_query_component(key)?, decode_query_component(value)?);
    }
    Ok(params)
}

fn decode_query_component(value: &str) -> std::result::Result<String, String> {
    let mut bytes = Vec::new();
    let mut chars = value.as_bytes().iter().copied();
    while let Some(byte) = chars.next() {
        push_decoded_byte(byte, &mut chars, &mut bytes)?;
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn push_decoded_byte(
    byte: u8,
    chars: &mut impl Iterator<Item = u8>,
    bytes: &mut Vec<u8>,
) -> std::result::Result<(), String> {
    if byte == b'+' {
        bytes.push(b' ');
        return Ok(());
    }
    if byte != b'%' {
        bytes.push(byte);
        return Ok(());
    }
    let high = chars
        .next()
        .ok_or_else(|| "incomplete percent escape".to_string())?;
    let low = chars
        .next()
        .ok_or_else(|| "incomplete percent escape".to_string())?;
    bytes.push(hex_byte(high, low)?);
    Ok(())
}

fn hex_byte(high: u8, low: u8) -> std::result::Result<u8, String> {
    Ok((hex_nibble(high)? << 4) | hex_nibble(low)?)
}

fn hex_nibble(byte: u8) -> std::result::Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(format!("invalid percent escape byte {other}")),
    }
}

pub(crate) struct HttpResponse {
    pub(crate) status: String,
    pub(crate) content_type: String,
    body: HttpResponseBody,
    pub(crate) headers: Vec<(String, String)>,
}

enum HttpResponseBody {
    Buffered(Vec<u8>),
    CompiledSse(CompiledSseRoute),
}

impl HttpResponse {
    pub(crate) fn empty(status: &str) -> Self {
        Self {
            status: status.to_string(),
            content_type: "application/json".to_string(),
            body: HttpResponseBody::Buffered(Vec::new()),
            headers: Vec::new(),
        }
    }

    pub(crate) fn buffered(
        status: impl Into<String>,
        content_type: impl Into<String>,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            status: status.into(),
            content_type: content_type.into(),
            body: HttpResponseBody::Buffered(body),
            headers,
        }
    }

    /// Returns the buffered body bytes, or `None` for streaming responses.
    pub(crate) fn buffered_bytes(&self) -> Option<&[u8]> {
        match &self.body {
            HttpResponseBody::Buffered(body) => Some(body.as_slice()),
            HttpResponseBody::CompiledSse(_) => None,
        }
    }

    pub(crate) fn compiled_sse(route: CompiledSseRoute) -> Self {
        Self {
            status: "200 OK".into(),
            content_type: "text/event-stream".into(),
            body: HttpResponseBody::CompiledSse(route),
            headers: Vec::new(),
        }
    }
}

pub(crate) fn json_response(status: &str, body: Value) -> HttpResponse {
    HttpResponse::buffered(
        status,
        "application/json",
        serde_json::to_vec(&body).unwrap_or_default(),
        Vec::new(),
    )
}

pub(crate) fn error_response(status: &str, message: impl ToString) -> HttpResponse {
    json_response(status, json!({ "error": message.to_string() }))
}

fn write_http_response(
    stream: &mut TcpStream,
    app: &AppCore,
    response: HttpResponse,
    shutdown: &AtomicBool,
) -> std::result::Result<(), std::io::Error> {
    let HttpResponse {
        status,
        content_type,
        body,
        headers,
    } = response;
    if let HttpResponseBody::CompiledSse(route) = body {
        return crate::plugin::compiled_surfaces::write_compiled_sse(
            stream,
            app.plugin_system(),
            route,
            shutdown,
        );
    }
    let HttpResponseBody::Buffered(body) = body else {
        unreachable!("compiled SSE response returned above")
    };
    write!(
        stream,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n",
        status,
        content_type,
        body.len()
    )?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.write_all(&body)
}

fn http_error(error: std::io::Error) -> AppCoreError {
    AppCoreError::unsupported(error.to_string(), "scoped MCP HTTP server")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Instant;

    #[test]
    fn hundred_concurrent_health_clients_receive_no_admission_rejection() {
        let app = Arc::new(AppCore::in_memory().unwrap());
        let server = ScopedMcpHttpServer::spawn(app).unwrap();
        let address = server
            .base_url()
            .strip_prefix("http://")
            .expect("loopback base URL")
            .to_owned();
        let (warmup_status, single_latency) = get_health(&address);
        assert_eq!(warmup_status, "200 OK");

        let clients = 100;
        let barrier = Arc::new(std::sync::Barrier::new(clients));
        let handles = (0..clients)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let address = address.clone();
                thread::spawn(move || {
                    barrier.wait();
                    get_health(&address)
                })
            })
            .collect::<Vec<_>>();
        let mut results = handles
            .into_iter()
            .map(|handle| handle.join().expect("health client panicked"))
            .collect::<Vec<_>>();
        assert!(
            results.iter().all(|(status, _)| status == "200 OK"),
            "all clients below configured admission must succeed: {results:?}"
        );
        results.sort_by_key(|(_, duration)| *duration);
        let p95 = results[(results.len() * 95).div_ceil(100) - 1].1;
        assert!(
            p95 < Duration::from_secs(1),
            "p95 GET /health latency exceeded async-server budget: {p95:?}"
        );
        println!(
            "http_performance workload=hundred_concurrent_health_clients single_latency_us={} parallel_clients={} parallel_p95_us={}",
            single_latency.as_micros(),
            clients,
            p95.as_micros(),
        );
    }

    fn get_health(address: &str) -> (String, Duration) {
        let started = Instant::now();
        let mut stream = TcpStream::connect(address).expect("connect health client");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set health read timeout");
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write health request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read health response");
        let status = response
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("HTTP/1.1 "))
            .unwrap_or_default()
            .to_owned();
        (status, started.elapsed())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn configured_admission_keeps_fast_requests_isolated_from_slow_requests() {
        let admission = Arc::new(HttpAdmission {
            permits: Arc::new(Semaphore::new(DEFAULT_HTTP_MAX_CONCURRENCY)),
            max_outstanding: DEFAULT_HTTP_MAX_CONCURRENCY + DEFAULT_HTTP_QUEUE_DEPTH,
            outstanding: AtomicUsize::new(0),
            enforce: true,
        });
        let slow_ready = Arc::new(tokio::sync::Barrier::new(21));
        let slow = (0..20)
            .map(|_| {
                let admission = Arc::clone(&admission);
                let slow_ready = Arc::clone(&slow_ready);
                tokio::spawn(async move {
                    let _permit = admission.acquire().await.expect("admit slow request");
                    slow_ready.wait().await;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                })
            })
            .collect::<Vec<_>>();
        slow_ready.wait().await;

        let fast = (0..80)
            .map(|_| {
                let admission = Arc::clone(&admission);
                tokio::spawn(async move {
                    let started = Instant::now();
                    let _permit = admission.acquire().await.expect("admit fast request");
                    started.elapsed()
                })
            })
            .collect::<Vec<_>>();
        let mut latencies = Vec::with_capacity(fast.len());
        for request in fast {
            latencies.push(request.await.expect("fast request task"));
        }
        latencies.sort();
        let p95 = latencies[(latencies.len() * 95).div_ceil(100) - 1];
        assert!(
            p95 < Duration::from_millis(50),
            "p95 fast request admission waited behind slow work: {p95:?}"
        );
        for request in slow {
            request.await.expect("slow request task");
        }
    }

    #[tokio::test]
    async fn change_stream_bypasses_http_admission() {
        let listener = AsyncTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Arc::new(AppCore::in_memory().unwrap());
        let admission = Arc::new(HttpAdmission {
            permits: Arc::new(Semaphore::new(0)),
            max_outstanding: 1,
            outstanding: AtomicUsize::new(0),
            enforce: true,
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let (shutdown_signal, shutdown_receiver) = watch::channel(false);

        let mut stream_client = AsyncTcpStream::connect(address).await.unwrap();
        let (stream_socket, _) = listener.accept().await.unwrap();
        let stream_task = tokio::spawn(serve_connection(
            Arc::clone(&app),
            stream_socket,
            Arc::clone(&admission),
            Arc::clone(&shutdown),
            shutdown_receiver.clone(),
        ));
        stream_client
            .write_all(b"GET /api/storage/changes/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut stream_response = [0_u8; 256];
        let read = tokio::time::timeout(
            Duration::from_millis(300),
            stream_client.read(&mut stream_response),
        )
        .await
        .expect("stream response promptly")
        .unwrap();
        assert!(
            String::from_utf8_lossy(&stream_response[..read]).contains("200 OK"),
            "exact stream route must bypass zero normal permits"
        );
        assert_eq!(admission.outstanding.load(Ordering::Acquire), 0);

        let mut normal_client = AsyncTcpStream::connect(address).await.unwrap();
        let (normal_socket, _) = listener.accept().await.unwrap();
        let normal_task = tokio::spawn(serve_connection(
            app,
            normal_socket,
            Arc::clone(&admission),
            shutdown,
            shutdown_receiver,
        ));
        normal_client
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(admission.outstanding.load(Ordering::Acquire), 1);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                normal_client.read(&mut [0_u8; 1]),
            )
            .await
            .is_err()
        );

        shutdown_signal.send(true).unwrap();
        drop(stream_client);
        stream_task.await.unwrap();
        normal_task.abort();
    }

    #[tokio::test]
    async fn invoke_v1_bypasses_generic_blocking_router() {
        let baseline = GENERIC_HTTP_ROUTER_INVOKE_V1_SPAWNS.load(Ordering::Acquire);
        let listener = AsyncTcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Arc::new(AppCore::in_memory().unwrap());
        let admission = HttpAdmission::from_environment();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (_shutdown_signal, shutdown_receiver) = watch::channel(false);

        let mut client = AsyncTcpStream::connect(address).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let server = tokio::spawn(serve_connection(
            app,
            socket,
            admission,
            shutdown,
            shutdown_receiver,
        ));
        client
            .write_all(
                b"POST /api/mcp/plugins/invoke-v1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut response))
            .await
            .expect("invalid invoke response promptly")
            .unwrap();
        server.await.unwrap();
        assert!(
            String::from_utf8_lossy(&response).contains("400 Bad Request"),
            "invoke-v1 validation must run on the dedicated path"
        );
        assert_eq!(
            GENERIC_HTTP_ROUTER_INVOKE_V1_SPAWNS.load(Ordering::Acquire),
            baseline,
            "invoke-v1 must not enter the generic spawn_blocking router"
        );
    }
    #[test]
    fn authorization_header_is_parsed_separately_from_query() {
        let (name, value) =
            parse_http_header("Authorization: Bearer generation-grant").expect("header");
        assert_eq!(name, "authorization");
        assert_eq!(value, "Bearer generation-grant");
        let query = BTreeMap::<String, String>::new();
        assert!(!query.contains_key("authorization"));
        assert!(!query.contains_key("__authorization"));
    }
}
