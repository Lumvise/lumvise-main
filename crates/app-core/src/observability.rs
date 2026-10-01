//! Structured logging (`tracing`) and metrics (Prometheus) for the daemon.
//!
//! This is the single host-process entry point for T7.1 observability:
//! - `init()` installs a global `tracing` subscriber and a global Prometheus
//!   metrics recorder. It is idempotent: the first caller wins, later calls are
//!   no-ops, so every binary entry point can call it unconditionally.
//! - Library crates instrument with the `metrics` facade macros and `tracing`
//!   macros directly; they are no-ops until a recorder/subscriber is installed
//!   here.
//!
//! Configuration (environment only, matching the existing boot convention):
//! - `RUST_LOG` — tracing level/target filter (default `info`).
//! - `LUMVISE_LOG_FORMAT` — `compact` (default) | `json` | `pretty` | `full`.
//! - `LUMVISE_METRICS_INTERVAL_SECS` — periodic gauge sampler interval
//!   (default `15`; `0` disables the sampler thread).
//! - `LUMVISE_METRICS` — `0` disables the Prometheus recorder entirely.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use metrics::{
    Unit, counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram,
};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

static METRICS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Installs the global tracing subscriber and Prometheus metrics recorder.
///
/// Call from every process entry point (desktop daemon, MCP stdio bridge).
/// Safe to call more than once: only the first call takes effect.
pub fn init() {
    install_tracing();
    install_metrics();
    describe_metrics();
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    let result = match log_format_from_env() {
        LogFormat::Json => registry.with(fmt::layer().json()).try_init(),
        LogFormat::Pretty => registry.with(fmt::layer().pretty()).try_init(),
        LogFormat::Full => registry.with(fmt::layer()).try_init(),
        LogFormat::Compact => registry.with(fmt::layer().compact()).try_init(),
    };
    // Ignore the "a global default subscriber has already been set" error —
    // the first caller to reach here owns observability for the process.
    let _ = result;
}

fn install_metrics() {
    if METRICS_HANDLE.get().is_some() {
        return;
    }
    if std::env::var("LUMVISE_METRICS").as_deref() == Ok("0") {
        return;
    }
    if let Ok(handle) = PrometheusBuilder::new().install_recorder() {
        let _ = METRICS_HANDLE.set(handle);
    }
}

/// Renders the Prometheus text exposition, or an empty string when metrics are
/// disabled. Backs the daemon `GET /metrics` route.
pub fn render_metrics() -> String {
    METRICS_HANDLE
        .get()
        .map(PrometheusHandle::render)
        .unwrap_or_default()
}

/// Interval at which the periodic gauge sampler should wake. `Duration::ZERO`
/// means "do not spawn the sampler".
pub fn sampler_interval() -> Duration {
    match std::env::var("LUMVISE_METRICS_INTERVAL_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        Some(0) => Duration::ZERO,
        Some(secs) => Duration::from_secs(secs),
        None => Duration::from_secs(15),
    }
}

// ---- HTTP request instrumentation -------------------------------------------

/// Records one served HTTP request: a monotonic counter and a latency histogram.
pub fn record_http_request(method: &str, route: &str, status: &str, elapsed: Duration) {
    counter!(
        "lumvise_http_requests_total",
        "method" => method.to_owned(),
        "status" => status_class(status).to_owned(),
    )
    .increment(1);
    histogram!(
        "lumvise_http_request_duration_seconds",
        "method" => method.to_owned(),
        "route" => route.to_owned(),
    )
    .record(elapsed.as_secs_f64());
}

/// Collapses a request path into a stable label so cardinality stays bounded.
pub fn route_metric_label(path: &str) -> String {
    let trimmed = path.split('?').next().unwrap_or(path);
    let mut segments = trimmed.trim_matches('/').split('/');
    match (segments.next(), segments.next()) {
        (Some("api"), Some(head)) => {
            let mut label = String::from("/api/");
            label.push_str(head);
            if segments.next().is_some() {
                label.push_str("/*");
            }
            label
        }
        (Some(first), _) if !first.is_empty() => format!("/{first}/*"),
        _ => "/".to_string(),
    }
}

fn status_class(status: &str) -> &'static str {
    if status.starts_with('2') {
        "2xx"
    } else if status.starts_with('3') {
        "3xx"
    } else if status.starts_with('4') {
        "4xx"
    } else if status.starts_with('5') {
        "5xx"
    } else {
        "other"
    }
}

// ---- Per-plugin mailbox instrumentation -------------------------------------

/// Sets the queued-depth and executing gauges for a plugin mailbox.
pub fn observe_plugin_mailbox(plugin_id: &str, executing: bool, queued: usize) {
    gauge!("lumvise_plugin_invocation_queued", "plugin" => plugin_id.to_owned()).set(queued as f64);
    gauge!("lumvise_plugin_invocation_executing", "plugin" => plugin_id.to_owned())
        .set(if executing { 1.0 } else { 0.0 });
}

// ---- Periodically-sampled gauges --------------------------------------------

/// Sets the maximum watermark lag for a registered graph-change hook.
pub fn observe_change_hook_watermark_lag(hook_name: &str, lag: i64) {
    gauge!(
        "lumvise_change_hook_watermark_lag",
        "hook" => hook_name.to_owned()
    )
    .set(lag.max(0) as f64);
}

/// Sets the number of coalesced entries currently retained by the hook buffer.
pub fn observe_change_hook_buffer_size(size: usize) {
    gauge!("lumvise_change_hook_buffer_size").set(size as f64);
}

/// Sets the total grafeo graph node count gauge.
pub fn observe_graph_node_count(count: usize) {
    gauge!("lumvise_graph_nodes").set(count as f64);
}

// ---- Writer-gate + startup instrumentation ----------------------------------

/// Records the time spent waiting to acquire a serializing DB writer gate.
pub fn record_writer_gate_wait(gate: &'static str, waited: Duration) {
    histogram!("lumvise_db_writer_gate_wait_seconds", "gate" => gate).record(waited.as_secs_f64());
}

/// Records the wall-clock duration of a named startup phase.
pub fn record_startup_phase_duration(phase: &str, elapsed: Duration) {
    histogram!("lumvise_startup_phase_duration_seconds", "phase" => phase.to_owned())
        .record(elapsed.as_secs_f64());
}

/// Convenience: record how long a closure took under a given startup phase.
pub fn time_startup_phase<F, R>(phase: &str, work: F) -> R
where
    F: FnOnce() -> R,
{
    let started = Instant::now();
    let result = work();
    record_startup_phase_duration(phase, started.elapsed());
    result
}

/// Records one sampled Assistant provider turn classification.
pub fn record_assistant_provider_turn(
    engine: &str,
    transport: &str,
    outcome: &str,
    code: Option<&str>,
) {
    counter!(
        "lumvise_assistant_provider_turns_total",
        "engine" => engine.to_owned(),
        "transport" => transport.to_owned(),
        "outcome" => outcome.to_owned(),
        "code" => code.unwrap_or("none").to_owned(),
    )
    .increment(1);
}

/// Records one sampled Assistant session terminal classification.
pub fn record_assistant_session(engine: &str, outcome: &str, code: Option<&str>) {
    counter!(
        "lumvise_assistant_sessions_total",
        "engine" => engine.to_owned(),
        "outcome" => outcome.to_owned(),
        "code" => code.unwrap_or("none").to_owned(),
    )
    .increment(1);
}

fn describe_metrics() {
    describe_counter!(
        "lumvise_http_requests_total",
        Unit::Count,
        "HTTP requests served"
    );
    describe_histogram!(
        "lumvise_http_request_duration_seconds",
        Unit::Seconds,
        "HTTP request handling latency"
    );
    describe_gauge!(
        "lumvise_plugin_invocation_queued",
        Unit::Count,
        "Plugin invocations waiting in the mailbox"
    );
    describe_gauge!(
        "lumvise_plugin_invocation_executing",
        Unit::Count,
        "Whether a plugin invocation owns dispatch (0/1)"
    );
    describe_counter!(
        "lumvise_assistant_provider_turns_total",
        Unit::Count,
        "Assistant provider turn outcomes"
    );
    describe_counter!(
        "lumvise_assistant_sessions_total",
        Unit::Count,
        "Assistant session terminal outcomes"
    );
    describe_counter!(
        "lumvise_assistant_tool_calls_total",
        Unit::Count,
        "Assistant MCP tool outcomes"
    );
    describe_gauge!(
        "lumvise_graph_nodes",
        Unit::Count,
        "Total grafeo graph nodes"
    );
    describe_histogram!(
        "lumvise_db_writer_gate_wait_seconds",
        Unit::Seconds,
        "Time spent waiting on a DB writer gate"
    );
    describe_histogram!(
        "lumvise_startup_phase_duration_seconds",
        Unit::Seconds,
        "Wall-clock duration of each startup phase"
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    Compact,
    Json,
    Pretty,
    Full,
}

fn log_format_from_env() -> LogFormat {
    match std::env::var("LUMVISE_LOG_FORMAT").ok().as_deref() {
        Some("json") => LogFormat::Json,
        Some("pretty") => LogFormat::Pretty,
        Some("full") => LogFormat::Full,
        _ => LogFormat::Compact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_label_collapses_dynamic_segments() {
        assert_eq!(route_metric_label("/health"), "/health/*");
        assert_eq!(route_metric_label("/api/context"), "/api/context");
        assert_eq!(
            route_metric_label("/api/knowledge/page"),
            "/api/knowledge/*"
        );
        assert_eq!(
            route_metric_label("/api/mcp/sessions/owner-1/sess-1"),
            "/api/mcp/*"
        );
        assert_eq!(route_metric_label("/"), "/");
        assert_eq!(route_metric_label("/api/context?x=1"), "/api/context");
    }

    #[test]
    fn status_class_buckets_by_hundreds() {
        assert_eq!(status_class("200 OK"), "2xx");
        assert_eq!(status_class("404 Not Found"), "4xx");
        assert_eq!(status_class("503 Service Unavailable"), "5xx");
        assert_eq!(status_class("nonsense"), "other");
    }
}
