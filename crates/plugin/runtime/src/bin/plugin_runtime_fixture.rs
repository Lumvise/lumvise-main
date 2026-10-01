use std::{
    collections::HashMap,
    io::{StdinLock, Write},
    sync::{Arc, Barrier, Mutex, mpsc},
    thread,
    time::Duration,
};

use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, WireMessage, WireOutcome,
};
use serde_json::json;

struct FixtureInvocation {
    invocation_id: String,
    capability_id: String,
    input: serde_json::Value,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fixture failed: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let plugin_id = installed_plugin_id()?;
    let codec = FrameCodec::default();
    let mut stdin = std::io::stdin().lock();
    let hello = codec
        .read_from(&mut stdin)
        .map_err(|error| error.to_string())?;
    let (session_id, package_digest) = hello_identity(hello)?;
    if plugin_id == "handshake-timeout" {
        thread::sleep(Duration::from_secs(10));
        return Ok(());
    }
    write_ready(
        &codec,
        &mut std::io::stdout().lock(),
        &plugin_id,
        &session_id,
        &package_digest,
    )?;
    // Drop the stdout lock before entering the mux path so worker threads
    // can lock stdout individually.
    if plugin_id.starts_with("mux-") {
        return serve_mux(codec, stdin, &session_id);
    }
    serve(
        &codec,
        &mut stdin,
        &mut std::io::stdout().lock(),
        &plugin_id,
        &session_id,
    )
}

fn installed_plugin_id() -> Result<String, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    executable
        .ancestors()
        .nth(3)
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("cannot derive plugin id from `{}`", executable.display()))
}

fn hello_identity(message: WireMessage) -> Result<(String, String), String> {
    match message.body {
        MessageBody::HostHello {
            session_id,
            package_digest,
            ..
        } => Ok((session_id, package_digest)),
        other => Err(format!("expected host hello, got `{other:?}`")),
    }
}

fn write_ready(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    installed_id: &str,
    session_id: &str,
    package_digest: &str,
) -> Result<(), String> {
    let plugin_id = if installed_id == "wrong-id" {
        "other-plugin"
    } else {
        installed_id
    };
    let package_digest = if installed_id == "wrong-digest" {
        "0".repeat(64)
    } else {
        package_digest.to_owned()
    };
    let ready = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginReady {
            session_id: session_id.to_owned(),
            plugin_id: plugin_id.to_owned(),
            package_digest,
        },
    };
    codec
        .write_to(stdout, &ready)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn serve(
    codec: &FrameCodec,
    stdin: &mut impl std::io::Read,
    stdout: &mut impl Write,
    plugin_id: &str,
    session_id: &str,
) -> Result<(), String> {
    loop {
        let message = codec.read_from(stdin).map_err(|error| error.to_string())?;
        match message.body {
            MessageBody::HostInvoke {
                invocation_id,
                capability_id,
                input,
                ..
            } => {
                respond_to_invocation(
                    codec,
                    stdin,
                    stdout,
                    plugin_id,
                    session_id,
                    FixtureInvocation {
                        invocation_id,
                        capability_id,
                        input,
                    },
                )?;
            }
            MessageBody::HostShutdown { reason, .. } => {
                return stop(codec, stdout, plugin_id, session_id, reason);
            }
            other => return Err(format!("unexpected fixture request `{other:?}`")),
        }
    }
}

/// Shared state for `mux-` fixture plugins: one worker thread per in-flight
/// invocation with serialized stdout writes, proving the host keeps the pipe
/// multiplexed. Non-`mux-` plugins keep the strictly sequential `serve` loop.
struct MuxShared {
    write_mutex: Mutex<()>,
    barriers: Mutex<HashMap<String, Arc<Barrier>>>,
    host_calls: Mutex<HashMap<String, mpsc::Sender<WireMessage>>>,
}

fn serve_mux(
    codec: FrameCodec,
    mut stdin: StdinLock<'static>,
    session_id: &str,
) -> Result<(), String> {
    let shared = Arc::new(MuxShared {
        write_mutex: Mutex::new(()),
        barriers: Mutex::new(HashMap::new()),
        host_calls: Mutex::new(HashMap::new()),
    });
    loop {
        let message = codec
            .read_from(&mut stdin)
            .map_err(|error| error.to_string())?;
        match message.body {
            MessageBody::HostInvoke {
                invocation_id,
                capability_id,
                input,
                ..
            } => {
                let shared = Arc::clone(&shared);
                let session_id = session_id.to_owned();
                thread::spawn(move || {
                    mux_invocation(
                        &shared,
                        &session_id,
                        FixtureInvocation {
                            invocation_id,
                            capability_id,
                            input,
                        },
                    );
                });
            }
            MessageBody::HostHostResult { ref call_id, .. } => {
                let waiter = shared
                    .host_calls
                    .lock()
                    .map_err(|_| "mux host-call lock poisoned".to_owned())?
                    .remove(call_id);
                if let Some(waiter) = waiter {
                    let _ = waiter.send(message);
                }
            }
            MessageBody::HostShutdown { reason, .. } => {
                let stopped = WireMessage {
                    protocol: CURRENT_PROTOCOL_VERSION,
                    body: MessageBody::PluginStopped {
                        session_id: session_id.to_owned(),
                        reason,
                    },
                };
                return mux_write(&shared, &stopped);
            }
            MessageBody::HostCancel { .. } => {
                // Host-side cancellation terminates the process; nothing to answer.
            }
            other => return Err(format!("unexpected mux fixture request `{other:?}`")),
        }
    }
}

fn mux_invocation(shared: &MuxShared, session_id: &str, invocation: FixtureInvocation) {
    let FixtureInvocation {
        invocation_id,
        capability_id,
        input,
    } = invocation;
    match capability_id.as_str() {
        // Blocks until a second invocation arrives on the same barrier name,
        // so a serialized pipe deadlocks this export until the deadline.
        "mux.barrier" => {
            let name = input["barrier"].as_str().unwrap_or("default").to_owned();
            let barrier = mux_barrier(shared, &name);
            if barrier.wait().is_leader()
                && let Ok(mut barriers) = shared.barriers.lock()
            {
                barriers.remove(&name);
            }
            let _ = mux_write_result(
                shared,
                session_id,
                invocation_id,
                json!({"capability_id": capability_id, "input": input}),
            );
        }
        "fixture.crash" => {
            eprintln!("fixture crash requested for invocation {invocation_id}");
            std::process::exit(23);
        }
        // Sends one host call per invocation (unique call id) and waits for the
        // routed result without blocking the fixture read loop.
        "mux.host-call" => mux_host_call(shared, session_id, invocation_id),
        _ => {
            let _ = mux_write_result(
                shared,
                session_id,
                invocation_id,
                json!({"capability_id": capability_id, "input": input}),
            );
        }
    }
}

fn mux_barrier(shared: &MuxShared, name: &str) -> Arc<Barrier> {
    let mut barriers = shared.barriers.lock().expect("mux barrier lock");
    Arc::clone(
        barriers
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(Barrier::new(2))),
    )
}

fn mux_host_call(shared: &MuxShared, session_id: &str, invocation_id: String) {
    let call_id = format!("{invocation_id}-call-1");
    let (sender, receiver) = mpsc::channel();
    shared
        .host_calls
        .lock()
        .expect("mux host-call lock")
        .insert(call_id.clone(), sender);
    let call = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginHostCall {
            session_id: session_id.to_owned(),
            invocation_id: invocation_id.clone(),
            call_id,
            capability_id: "clock.read".to_owned(),
            input: json!({"timezone": "UTC"}),
        },
    };
    if mux_write(shared, &call).is_err() {
        return;
    }
    let Ok(response) = receiver.recv() else {
        return;
    };
    let MessageBody::HostHostResult { outcome, .. } = response.body else {
        return;
    };
    let _ = mux_write_result(
        shared,
        session_id,
        invocation_id,
        json!({"host_outcome": outcome}),
    );
}

fn mux_write_result(
    shared: &MuxShared,
    session_id: &str,
    invocation_id: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.to_owned(),
            invocation_id,
            outcome: WireOutcome::Succeeded { value },
        },
    };
    mux_write(shared, &result)
}

fn mux_write(shared: &MuxShared, message: &WireMessage) -> Result<(), String> {
    let _guard = shared
        .write_mutex
        .lock()
        .map_err(|_| "mux write lock poisoned".to_owned())?;
    let mut stdout = std::io::stdout().lock();
    FrameCodec::default()
        .write_to(&mut stdout, message)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn respond_to_invocation(
    codec: &FrameCodec,
    stdin: &mut impl std::io::Read,
    stdout: &mut impl Write,
    plugin_id: &str,
    session_id: &str,
    invocation: FixtureInvocation,
) -> Result<(), String> {
    let FixtureInvocation {
        invocation_id,
        capability_id,
        input,
    } = invocation;
    if plugin_id == "crash" || capability_id == "fixture.crash" {
        eprintln!("fixture crash requested for invocation {invocation_id}");
        std::process::exit(23);
    }
    if plugin_id == "timeout"
        && matches!(
            capability_id.as_str(),
            "fixture.timeout" | "fixture.recurring" | "target.echo"
        )
    {
        thread::sleep(Duration::from_secs(10));
        return Ok(());
    }
    if plugin_id.starts_with("sse-") {
        return sse_invocation(codec, stdout, plugin_id, session_id, invocation_id, input);
    }
    if plugin_id == "schema-guard"
        && input
            .pointer("/profile/name")
            .and_then(|name| name.as_str())
            .is_none()
    {
        eprintln!("schema guard received invalid input");
        std::process::exit(31);
    }
    if plugin_id.starts_with("host-call-") {
        return host_call_invocation(codec, stdin, stdout, plugin_id, session_id, invocation_id);
    }
    if plugin_id.starts_with("plugin-invoke-") {
        return plugin_invoke_invocation(codec, stdin, stdout, session_id, invocation_id, input);
    }
    if plugin_id == "target-failed" {
        return write_failed_invocation(codec, stdout, session_id, invocation_id);
    }
    if capability_id == "lane.acquire" && input["force_fail"] == true {
        return write_failed_invocation(codec, stdout, session_id, invocation_id);
    }
    if capability_id == "project_knowledge"
        && matches!(plugin_id, "builtin.knowledge" | "builtin.nucleus")
    {
        return projection_invocation(codec, stdout, plugin_id, session_id, invocation_id, input);
    }
    if plugin_id == "sandbox-read-denied" {
        return sandbox_read_result(codec, stdout, session_id, invocation_id, input);
    }
    let invocation_id = if plugin_id == "wrong-correlation" {
        "different-invocation".to_owned()
    } else {
        invocation_id
    };
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.to_owned(),
            invocation_id,
            outcome: WireOutcome::Succeeded {
                value: json!({"capability_id": capability_id, "input": input}),
            },
        },
    };
    codec
        .write_to(stdout, &result)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn projection_invocation(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    plugin_id: &str,
    session_id: &str,
    invocation_id: String,
    input: serde_json::Value,
) -> Result<(), String> {
    if input["projectRoot"] == "/crash" && plugin_id == "builtin.knowledge" {
        std::process::exit(29);
    }
    if input["projectRoot"] == "/invalid" && plugin_id == "builtin.knowledge" {
        return write_projection_result(
            codec,
            stdout,
            session_id,
            invocation_id,
            json!({"spaces": "invalid", "elements": []}),
        );
    }
    let (space, policy) = if plugin_id == "builtin.knowledge" {
        ("project", "annotation")
    } else {
        ("nuclei", "readonly")
    };
    let value = fixture_projection(plugin_id, space, policy, &input);
    write_projection_result(codec, stdout, session_id, invocation_id, value)
}

fn write_projection_result(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    session_id: &str,
    invocation_id: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.into(),
            invocation_id,
            outcome: WireOutcome::Succeeded { value },
        },
    };
    codec
        .write_to(stdout, &result)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn fixture_projection(
    plugin_id: &str,
    space: &str,
    write_policy: &str,
    input: &serde_json::Value,
) -> serde_json::Value {
    let element_id = format!("{plugin_id}:projection");
    json!({"spaces": [{"spaceId": space, "title": space, "status": "ready",
        "writePolicy": write_policy}], "elements": [{"elementId": element_id,
        "space": space, "kind": "fixture", "title": plugin_id, "markdown": "# Signed projection",
        "contentMd5": "0123456789abcdef0123456789abcdef", "pathHint": format!("{space}.md"),
        "sourceId": input["projectRoot"], "sourceRefs": [], "syncToken": "fixture:1",
        "changeMarker": "fixture:1", "ownership": {"content": "lumvise", "source": "lumvise"},
        "writePolicy": write_policy, "children": [], "artifacts": [], "properties": {}}]})
}

fn sse_invocation(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    plugin_id: &str,
    session_id: &str,
    invocation_id: String,
    input: serde_json::Value,
) -> Result<(), String> {
    if plugin_id == "sse-crash" {
        std::process::exit(24);
    }
    let value = match plugin_id {
        "sse-malformed" => json!({"events": "invalid", "next_cursor": null, "done": false}),
        "sse-never" => json!({"events": [], "next_cursor": input["cursor"], "done": false}),
        _ => fixture_sse_page(input["cursor"].as_str()),
    };
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.to_owned(),
            invocation_id,
            outcome: WireOutcome::Succeeded { value },
        },
    };
    codec
        .write_to(stdout, &result)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn fixture_sse_page(cursor: Option<&str>) -> serde_json::Value {
    let (sequence, done) = match cursor {
        None => (1, false),
        Some("cursor-1") => (2, false),
        _ => (3, true),
    };
    json!({
        "events": [{
            "id": format!("event-{sequence}"),
            "event": "fixture.changed",
            "data": {"sequence": sequence},
            "retry_ms": 10
        }],
        "next_cursor": format!("cursor-{sequence}"),
        "done": done
    })
}

fn plugin_invoke_invocation(
    codec: &FrameCodec,
    stdin: &mut impl std::io::Read,
    stdout: &mut impl Write,
    session_id: &str,
    invocation_id: String,
    input: serde_json::Value,
) -> Result<(), String> {
    let call = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginHostCall {
            session_id: session_id.to_owned(),
            invocation_id: invocation_id.clone(),
            call_id: "plugin-invoke-1".to_owned(),
            capability_id: "plugin.invoke".to_owned(),
            input,
        },
    };
    codec
        .write_to(&mut *stdout, &call)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())?;
    let response = codec.read_from(stdin).map_err(|error| error.to_string())?;
    let MessageBody::HostHostResult { outcome, .. } = response.body else {
        return Err("expected plugin.invoke host result".to_owned());
    };
    write_terminal_host_outcome(codec, stdout, session_id, invocation_id, outcome)
}

fn write_failed_invocation(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    session_id: &str,
    invocation_id: String,
) -> Result<(), String> {
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.to_owned(),
            invocation_id,
            outcome: WireOutcome::Failed {
                error: lumvise_plugin_protocol::PluginWireError {
                    code: "target_application_failed".to_owned(),
                    message: "fixture application failure".to_owned(),
                    details: None,
                    retryable: false,
                },
            },
        },
    };
    codec
        .write_to(stdout, &result)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn sandbox_read_result(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    session_id: &str,
    invocation_id: String,
    input: serde_json::Value,
) -> Result<(), String> {
    let path = input["path"]
        .as_str()
        .ok_or_else(|| "sandbox read fixture requires string `path`".to_owned())?;
    let value = match std::fs::read(path) {
        Ok(_) => json!({"read_error_kind": null}),
        Err(error) => json!({"read_error_kind": error_kind_name(error.kind())}),
    };
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.to_owned(),
            invocation_id,
            outcome: WireOutcome::Succeeded { value },
        },
    };
    codec
        .write_to(&mut *stdout, &result)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn error_kind_name(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::NotFound => "not_found",
        _ => "other",
    }
}

fn host_call_invocation(
    codec: &FrameCodec,
    stdin: &mut impl std::io::Read,
    stdout: &mut impl Write,
    plugin_id: &str,
    session_id: &str,
    invocation_id: String,
) -> Result<(), String> {
    let returned_session = if plugin_id == "host-call-session-mismatch" {
        "different-session"
    } else {
        session_id
    };
    let returned_invocation = if plugin_id == "host-call-parent-mismatch" {
        "different-invocation"
    } else {
        &invocation_id
    };
    let capability_id = if plugin_id == "host-call-undeclared" {
        "filesystem.read"
    } else {
        "clock.read"
    };
    let host_call = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginHostCall {
            session_id: returned_session.to_owned(),
            invocation_id: returned_invocation.to_owned(),
            call_id: "host-call-1".to_owned(),
            capability_id: capability_id.to_owned(),
            input: json!({"timezone": "UTC"}),
        },
    };
    codec
        .write_to(&mut *stdout, &host_call)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())?;
    let response = codec.read_from(stdin).map_err(|error| error.to_string())?;
    if plugin_id == "host-call-reentrant" {
        codec
            .write_to(&mut *stdout, &host_call)
            .map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;
        return codec
            .read_from(stdin)
            .map(|_| ())
            .map_err(|error| error.to_string());
    }
    let outcome = match response.body {
        MessageBody::HostHostResult { outcome, .. } => outcome,
        other => return Err(format!("expected host result, got `{other:?}`")),
    };
    write_terminal_host_outcome(codec, stdout, session_id, invocation_id, outcome)
}

fn write_terminal_host_outcome(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    session_id: &str,
    invocation_id: String,
    host_outcome: WireOutcome,
) -> Result<(), String> {
    let result = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: session_id.to_owned(),
            invocation_id,
            outcome: WireOutcome::Succeeded {
                value: json!({"host_outcome": host_outcome}),
            },
        },
    };
    codec
        .write_to(stdout, &result)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}

fn stop(
    codec: &FrameCodec,
    stdout: &mut impl Write,
    plugin_id: &str,
    session_id: &str,
    reason: Option<String>,
) -> Result<(), String> {
    if plugin_id == "ignore-shutdown" {
        thread::sleep(Duration::from_secs(10));
        return Ok(());
    }
    let stopped = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginStopped {
            session_id: session_id.to_owned(),
            reason,
        },
    };
    codec
        .write_to(stdout, &stopped)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())
}
