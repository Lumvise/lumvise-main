//! Server-sent event delivery for signed compiled HTTP exports.

use std::{
    collections::BTreeSet,
    io::Write,
    net::TcpStream,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use lumvise_plugin_package::SseStreamPolicy;
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_runtime::{PluginInvocationError, PluginSystem};
use serde::Deserialize;
use serde_json::{Value, json};

use super::invocation::surface_invocation_request;

pub(crate) struct CompiledSseRoute {
    pub(crate) plugin_id: String,
    pub(crate) export_id: String,
    pub(crate) input: Value,
    pub(crate) policy: SseStreamPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SsePollEnvelope {
    events: Vec<SseEvent>,
    next_cursor: Option<String>,
    done: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SseEvent {
    id: String,
    event: String,
    data: Value,
    retry_ms: Option<u64>,
}

struct SseDeliveryState {
    cursor: Option<String>,
    last_write: Instant,
    seen_event_ids: BTreeSet<String>,
}

pub(crate) fn write_compiled_sse(
    stream: &mut TcpStream,
    system: &PluginSystem,
    route: CompiledSseRoute,
    shutdown: &AtomicBool,
) -> std::io::Result<()> {
    write_sse_headers(stream)?;
    let mut state = delivery_state(&route);
    while stream_is_live(stream, system, &route, shutdown) {
        let envelope = poll_plugin(system, &route, &state);
        let envelope = match envelope {
            Ok(envelope) => envelope,
            Err(message) => return write_terminal_error(stream, &mut state, &message),
        };
        let delay = match deliver_poll(stream, &route, &mut state, envelope) {
            Ok(delay) => delay,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return write_terminal_error(stream, &mut state, &error.to_string());
            }
            Err(error) => return Err(error),
        };
        if delay.is_zero()
            || !interruptible_wait(stream, system, &route, shutdown, &mut state, delay)?
        {
            return Ok(());
        }
    }
    Ok(())
}

fn delivery_state(route: &CompiledSseRoute) -> SseDeliveryState {
    SseDeliveryState {
        cursor: initial_cursor(&route.input),
        last_write: Instant::now(),
        seen_event_ids: BTreeSet::new(),
    }
}

fn poll_plugin(
    system: &PluginSystem,
    route: &CompiledSseRoute,
    state: &SseDeliveryState,
) -> Result<SsePollEnvelope, String> {
    let input = polling_input(route, state);
    let request = surface_invocation_request(
        &route.plugin_id,
        &route.export_id,
        input,
        "compiled-sse",
        Instant::now() + lumvise_plugin_runtime::PLUGIN_INVOCATION_DEADLINE,
    );
    let outcome = system.invoke_controlled(request);
    poll_envelope(outcome)
}

fn write_sse_headers(stream: &mut TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nX-Content-Type-Options: nosniff\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
    )?;
    stream.flush()
}

fn initial_cursor(input: &Value) -> Option<String> {
    input.pointer("/query/cursor")?.as_str().map(str::to_owned)
}

fn requested_event_count(input: &Value, maximum: u32) -> u32 {
    input
        .pointer("/query/max_events")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .map_or(maximum, |value| value.min(maximum))
}

fn polling_input(route: &CompiledSseRoute, state: &SseDeliveryState) -> Value {
    let mut input = route.input.clone();
    let Some(object) = input.as_object_mut() else {
        return input;
    };
    object.insert(
        "cursor".into(),
        state.cursor.clone().map_or(Value::Null, Value::String),
    );
    object.insert(
        "max_events".into(),
        requested_event_count(&route.input, route.policy.max_events_per_poll).into(),
    );
    input
}

fn poll_envelope(
    outcome: Result<WireOutcome, PluginInvocationError>,
) -> Result<SsePollEnvelope, String> {
    match outcome {
        Ok(WireOutcome::Succeeded { value }) => serde_json::from_value(value)
            .map_err(|error| format!("invalid signed SSE envelope: {error}")),
        Ok(WireOutcome::Failed { error }) => Err(format!(
            "plugin SSE poll failed with `{}`: {}",
            error.code, error.message
        )),
        Err(error) => Err(format!("plugin SSE poll failed: {error}")),
    }
}

fn deliver_poll(
    stream: &mut TcpStream,
    route: &CompiledSseRoute,
    state: &mut SseDeliveryState,
    envelope: SsePollEnvelope,
) -> std::io::Result<Duration> {
    validate_poll_size(route, &envelope)?;
    validate_poll_progress(state, &envelope)?;
    let delay = event_backoff(route, &envelope.events)?;
    for event in envelope.events {
        deliver_event(stream, state, event)?;
    }
    state.cursor = envelope.next_cursor;
    Ok(if envelope.done { Duration::ZERO } else { delay })
}

fn validate_poll_size(route: &CompiledSseRoute, envelope: &SsePollEnvelope) -> std::io::Result<()> {
    if envelope.events.len() > route.policy.max_events_per_poll as usize {
        return Err(invalid_contract("SSE poll exceeded signed event quota"));
    }
    Ok(())
}

fn validate_poll_progress(
    state: &mut SseDeliveryState,
    envelope: &SsePollEnvelope,
) -> std::io::Result<()> {
    if !envelope.events.is_empty() && envelope.next_cursor == state.cursor {
        return Err(invalid_contract(
            "nonempty SSE poll did not advance its cursor",
        ));
    }
    for event in &envelope.events {
        if state.seen_event_ids.contains(&event.id) {
            return Err(invalid_contract("SSE poll replayed an event id"));
        }
    }
    state
        .seen_event_ids
        .extend(envelope.events.iter().map(|event| event.id.clone()));
    Ok(())
}

fn event_backoff(route: &CompiledSseRoute, events: &[SseEvent]) -> std::io::Result<Duration> {
    let requested = events.iter().filter_map(|event| event.retry_ms).max();
    if requested.is_some_and(|delay| delay > route.policy.max_backoff_ms) {
        return Err(invalid_contract(
            "plugin SSE retry exceeds signed max_backoff_ms",
        ));
    }
    Ok(Duration::from_millis(
        requested.unwrap_or(route.policy.poll_interval_ms),
    ))
}

fn deliver_event(
    stream: &mut TcpStream,
    state: &mut SseDeliveryState,
    event: SseEvent,
) -> std::io::Result<()> {
    let frame = encode_event(&event)?;
    write_bytes(stream, state, frame.as_bytes())?;
    Ok(())
}

fn encode_event(event: &SseEvent) -> std::io::Result<String> {
    if contains_newline(&event.id) || contains_newline(&event.event) {
        return Err(invalid_contract("plugin SSE id/event contains a newline"));
    }
    let mut frame = format!("id: {}\nevent: {}\n", event.id, event.event);
    if let Some(retry_ms) = event.retry_ms {
        frame.push_str(&format!("retry: {retry_ms}\n"));
    }
    frame.push_str("data: ");
    frame.push_str(&serde_json::to_string(&event.data).map_err(invalid_data)?);
    frame.push_str("\n\n");
    Ok(frame)
}

fn contains_newline(value: &str) -> bool {
    value.contains(['\r', '\n'])
}

fn invalid_data(error: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

fn invalid_contract(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn write_bytes(
    stream: &mut TcpStream,
    state: &mut SseDeliveryState,
    bytes: &[u8],
) -> std::io::Result<()> {
    stream.write_all(bytes)?;
    stream.flush()?;
    state.last_write = Instant::now();
    Ok(())
}

fn write_terminal_error(
    stream: &mut TcpStream,
    state: &mut SseDeliveryState,
    message: &str,
) -> std::io::Result<()> {
    let body = serde_json::to_string(&json!({"message": message})).map_err(invalid_data)?;
    let frame = format!("event: error\ndata: {body}\n\n");
    write_bytes(stream, state, frame.as_bytes())
}

fn stream_is_live(
    stream: &TcpStream,
    system: &PluginSystem,
    route: &CompiledSseRoute,
    shutdown: &AtomicBool,
) -> bool {
    !shutdown.load(Ordering::Acquire)
        && system.is_active(&route.plugin_id).unwrap_or(false)
        && !client_disconnected(stream)
}

fn client_disconnected(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0_u8; 1];
    let result = match stream.peek(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(_) => true,
    };
    let _ = stream.set_nonblocking(false);
    result
}

fn interruptible_wait(
    stream: &mut TcpStream,
    system: &PluginSystem,
    route: &CompiledSseRoute,
    shutdown: &AtomicBool,
    state: &mut SseDeliveryState,
    delay: Duration,
) -> std::io::Result<bool> {
    const CANCELLATION_SLICE: Duration = Duration::from_millis(25);
    let wait_started = Instant::now();
    while wait_started.elapsed() < delay {
        if !stream_is_live(stream, system, route, shutdown) {
            return Ok(false);
        }
        if state.last_write.elapsed() >= Duration::from_millis(route.policy.heartbeat_interval_ms) {
            write_bytes(stream, state, b": heartbeat\n\n")?;
        }
        let remaining = delay.saturating_sub(wait_started.elapsed());
        std::thread::sleep(CANCELLATION_SLICE.min(remaining));
    }
    Ok(true)
}
