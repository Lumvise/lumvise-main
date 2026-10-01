use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, WireMessage, WireOutcome,
};
use serde_json::Value;

use crate::context::HostCallTransport;
use crate::{PluginApplication, PluginContext, PluginError, SdkError};

struct Handshake {
    session_id: String,
    package_digest: String,
}

struct Invocation {
    id: String,
    capability_id: String,
    input: Value,
}

struct InvocationControl {
    cancelled: AtomicBool,
    active_call_id: Mutex<Option<String>>,
}

impl InvocationControl {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            active_call_id: Mutex::new(None),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

enum HostResponse {
    Result {
        session_id: String,
        call_id: String,
        outcome: WireOutcome,
    },
    Cancel,
    Shutdown,
}

/// Runs one plugin session over separate blocking input and output byte streams.
///
/// The SDK multiplexes each host invocation onto an independent worker while keeping one reader
/// responsible for protocol ordering and one mutex-protected writer for complete stdout frames.
/// This permits a foreground invocation to proceed while another invocation waits on a host call.
///
/// # Errors
///
/// Returns [`SdkError`] when framing, handshake, session correlation, or message sequencing is
/// invalid. Application failures are returned to the host as structured plugin results.
pub fn run<R, W, A>(reader: &mut R, writer: &mut W, application: &A) -> Result<(), SdkError>
where
    R: Read,
    W: Write + Send,
    A: PluginApplication + Sync,
{
    let codec = FrameCodec::default();
    let handshake = receive_hello(reader, &codec)?;
    send_ready(writer, &codec, application, &handshake)?;
    run_session(reader, writer, application, &codec, &handshake.session_id)
}

/// Runs one plugin session over locked process standard input and output.
///
/// # Errors
///
/// Returns the same session-level failures as [`run`].
pub fn run_stdio<A>(application: &A) -> Result<(), SdkError>
where
    A: PluginApplication + Sync,
{
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    run(&mut stdin.lock(), &mut stdout, application)
}

fn receive_hello<R: Read>(reader: &mut R, codec: &FrameCodec) -> Result<Handshake, SdkError> {
    let message = codec.read_from(reader)?;
    match message.body {
        MessageBody::HostHello {
            session_id,
            package_digest,
            ..
        } => Ok(Handshake {
            session_id,
            package_digest,
        }),
        body => Err(unexpected(&body, "host.hello as the first message")),
    }
}

fn send_ready<W: Write>(
    writer: &mut W,
    codec: &FrameCodec,
    application: &impl PluginApplication,
    handshake: &Handshake,
) -> Result<(), SdkError> {
    write_body(
        writer,
        codec,
        MessageBody::PluginReady {
            session_id: handshake.session_id.clone(),
            plugin_id: application.plugin_id().into(),
            package_digest: handshake.package_digest.clone(),
        },
    )
}

fn run_session<R, W, A>(
    reader: &mut R,
    writer: &mut W,
    application: &A,
    codec: &FrameCodec,
    session_id: &str,
) -> Result<(), SdkError>
where
    R: Read,
    W: Write + Send,
    A: PluginApplication + Sync,
{
    let writer = Arc::new(Mutex::new(writer));
    let controls = Arc::new(Mutex::new(HashMap::<String, Arc<InvocationControl>>::new()));
    let pending = Arc::new(Mutex::new(
        HashMap::<String, SyncSender<HostResponse>>::new(),
    ));
    let shutdown_requested = Arc::new(AtomicBool::new(false));

    thread::scope(|scope| {
        let mut workers = Vec::new();
        loop {
            let message = match codec.read_from(reader) {
                Ok(message) => message,
                Err(error) => {
                    shutdown_requested.store(true, Ordering::Release);
                    let _ = close_pending_calls(&pending);
                    return Err(error.into());
                }
            };
            match message.body {
                MessageBody::HostInvoke {
                    session_id: actual,
                    invocation_id,
                    capability_id,
                    input,
                } => {
                    validate_session_or_close(&actual, session_id, &pending, &shutdown_requested)?;
                    let control = Arc::new(InvocationControl::new());
                    lock_session(&controls, "invocation controls")?
                        .insert(invocation_id.clone(), Arc::clone(&control));
                    let writer = Arc::clone(&writer);
                    let controls = Arc::clone(&controls);
                    let pending = Arc::clone(&pending);
                    let shutdown_requested = Arc::clone(&shutdown_requested);
                    let session_id = session_id.to_owned();
                    let codec = *codec;
                    workers.push(scope.spawn(move || {
                        run_invocation(
                            application,
                            Invocation {
                                id: invocation_id.clone(),
                                capability_id,
                                input,
                            },
                            &session_id,
                            codec,
                            writer,
                            Arc::clone(&control),
                            pending,
                            shutdown_requested,
                        );
                        if let Ok(mut controls) = controls.lock() {
                            controls.remove(&invocation_id);
                        }
                    }));
                }
                MessageBody::HostHostResult {
                    session_id: actual,
                    call_id,
                    outcome,
                } => {
                    validate_session_or_close(&actual, session_id, &pending, &shutdown_requested)?;
                    let sender = lock_session(&pending, "host calls")?.get(&call_id).cloned();
                    let Some(sender) = sender else {
                        shutdown_requested.store(true, Ordering::Release);
                        close_pending_calls(&pending)?;
                        return Err(SdkError::UnexpectedMessage {
                            actual: "host.host_result",
                            expected: "a pending matching host call",
                        });
                    };
                    let _ = sender.send(HostResponse::Result {
                        session_id: actual,
                        call_id,
                        outcome,
                    });
                }
                MessageBody::HostCancel {
                    session_id: actual,
                    invocation_id,
                } => {
                    validate_session_or_close(&actual, session_id, &pending, &shutdown_requested)?;
                    if let Some(control) = lock_session(&controls, "invocation controls")?
                        .get(&invocation_id)
                        .cloned()
                    {
                        control.cancel();
                        if let Some(call_id) =
                            lock_session(&control.active_call_id, "active host call")?.clone()
                            && let Some(sender) =
                                lock_session(&pending, "host calls")?.remove(&call_id)
                        {
                            let _ = sender.send(HostResponse::Cancel);
                        }
                    }
                }
                MessageBody::HostShutdown {
                    session_id: actual,
                    reason,
                } => {
                    validate_session_or_close(&actual, session_id, &pending, &shutdown_requested)?;
                    shutdown_requested.store(true, Ordering::Release);
                    for (_, sender) in lock_session(&pending, "host calls")?.drain() {
                        let _ = sender.send(HostResponse::Shutdown);
                    }
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return send_stopped_shared(&writer, codec, session_id, reason);
                }
                body => {
                    shutdown_requested.store(true, Ordering::Release);
                    close_pending_calls(&pending)?;
                    return Err(unexpected(
                        &body,
                        "host.invoke, host.host_result, host.cancel, or host.shutdown",
                    ));
                }
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn run_invocation<W: Write + Send>(
    application: &impl PluginApplication,
    invocation: Invocation,
    session_id: &str,
    codec: FrameCodec,
    writer: Arc<Mutex<&mut W>>,
    control: Arc<InvocationControl>,
    pending: Arc<Mutex<HashMap<String, SyncSender<HostResponse>>>>,
    shutdown_requested: Arc<AtomicBool>,
) {
    let outcome = {
        let mut channel = MultiplexedHostCallChannel::new(
            codec,
            session_id,
            &invocation.id,
            Arc::clone(&writer),
            Arc::clone(&control),
            pending,
            shutdown_requested,
        );
        let mut context = PluginContext::new(&mut channel);
        application.dispatch(&invocation.capability_id, invocation.input, &mut context)
    };
    let _ = send_result_shared(&writer, codec, session_id, &invocation.id, outcome);
}

struct MultiplexedHostCallChannel<'session, 'writer, W: Write + Send> {
    codec: FrameCodec,
    session_id: &'session str,
    invocation_id: &'session str,
    next_call_number: u64,
    writer: Arc<Mutex<&'writer mut W>>,
    control: Arc<InvocationControl>,
    pending: Arc<Mutex<HashMap<String, SyncSender<HostResponse>>>>,
    shutdown_requested: Arc<AtomicBool>,
}

impl<'session, 'writer, W: Write + Send> MultiplexedHostCallChannel<'session, 'writer, W> {
    fn new(
        codec: FrameCodec,
        session_id: &'session str,
        invocation_id: &'session str,
        writer: Arc<Mutex<&'writer mut W>>,
        control: Arc<InvocationControl>,
        pending: Arc<Mutex<HashMap<String, SyncSender<HostResponse>>>>,
        shutdown_requested: Arc<AtomicBool>,
    ) -> Self {
        Self {
            codec,
            session_id,
            invocation_id,
            next_call_number: 1,
            writer,
            control,
            pending,
            shutdown_requested,
        }
    }

    fn next_call_id(&mut self) -> String {
        let call_id = format!("{}:host-call:{}", self.invocation_id, self.next_call_number);
        self.next_call_number += 1;
        call_id
    }

    fn clear_call(&self, call_id: &str) -> Result<(), PluginError> {
        lock_plugin(&self.pending, "host calls")?.remove(call_id);
        let mut active = lock_plugin(&self.control.active_call_id, "active host call")?;
        if active.as_deref() == Some(call_id) {
            *active = None;
        }
        Ok(())
    }

    fn validate_result(
        &self,
        expected_call_id: &str,
        response: HostResponse,
    ) -> Result<Value, PluginError> {
        match response {
            HostResponse::Result {
                session_id,
                call_id,
                outcome,
            } => {
                validate_host_correlation("session_id", &session_id, self.session_id)?;
                validate_host_correlation("call_id", &call_id, expected_call_id)?;
                match outcome {
                    WireOutcome::Succeeded { value } => Ok(value),
                    WireOutcome::Failed { error } => Err(error.into()),
                }
            }
            HostResponse::Cancel => Err(PluginError::canceled(self.invocation_id)),
            HostResponse::Shutdown => Err(PluginError::new(
                "shutdown_requested",
                "host requested shutdown",
                false,
            )),
        }
    }
}

impl<W: Write + Send> HostCallTransport for MultiplexedHostCallChannel<'_, '_, W> {
    fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError> {
        if self.control.is_cancelled() {
            return Err(PluginError::canceled(self.invocation_id));
        }
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(PluginError::new(
                "shutdown_requested",
                "host requested shutdown",
                false,
            ));
        }
        let call_id = self.next_call_id();
        let (sender, receiver): (SyncSender<HostResponse>, Receiver<HostResponse>) =
            mpsc::sync_channel(1);
        *lock_plugin(&self.control.active_call_id, "active host call")? = Some(call_id.clone());
        lock_plugin(&self.pending, "host calls")?.insert(call_id.clone(), sender);
        if let Err(error) = send_body_shared(
            &self.writer,
            self.codec,
            MessageBody::PluginHostCall {
                session_id: self.session_id.into(),
                invocation_id: self.invocation_id.into(),
                call_id: call_id.clone(),
                capability_id: capability_id.into(),
                input,
            },
        ) {
            let _ = self.clear_call(&call_id);
            return Err(protocol_as_plugin_error(error));
        }
        let response = receiver.recv().map_err(|_| {
            PluginError::degraded_protocol("host stopped before responding to the active host call")
        });
        self.clear_call(&call_id)?;
        response.and_then(|response| self.validate_result(&call_id, response))
    }
}

fn send_result_shared<W: Write + Send>(
    writer: &Arc<Mutex<&mut W>>,
    codec: FrameCodec,
    session_id: &str,
    invocation_id: &str,
    result: Result<Value, PluginError>,
) -> Result<(), SdkError> {
    let outcome = match result {
        Ok(value) => WireOutcome::Succeeded { value },
        Err(error) => WireOutcome::Failed {
            error: error.into(),
        },
    };
    send_body_shared(
        writer,
        codec,
        MessageBody::PluginResult {
            session_id: session_id.into(),
            invocation_id: invocation_id.into(),
            outcome,
        },
    )
}

fn send_stopped_shared<W: Write + Send>(
    writer: &Arc<Mutex<&mut W>>,
    codec: &FrameCodec,
    session_id: &str,
    reason: Option<String>,
) -> Result<(), SdkError> {
    send_body_shared(
        writer,
        *codec,
        MessageBody::PluginStopped {
            session_id: session_id.into(),
            reason,
        },
    )
}

fn send_body_shared<W: Write + Send>(
    writer: &Arc<Mutex<&mut W>>,
    codec: FrameCodec,
    body: MessageBody,
) -> Result<(), SdkError> {
    let mut writer = lock_session(writer, "stdout")?;
    write_body(&mut **writer, &codec, body)
}

fn lock_session<'a, T>(
    mutex: &'a Mutex<T>,
    resource: &'static str,
) -> Result<MutexGuard<'a, T>, SdkError> {
    mutex
        .lock()
        .map_err(|_| SdkError::SessionState { resource })
}

fn lock_plugin<'a, T>(
    mutex: &'a Mutex<T>,
    resource: &'static str,
) -> Result<MutexGuard<'a, T>, PluginError> {
    mutex
        .lock()
        .map_err(|_| PluginError::degraded_protocol(format!("{resource} mutex is poisoned")))
}

fn close_pending_calls(
    pending: &Mutex<HashMap<String, SyncSender<HostResponse>>>,
) -> Result<(), SdkError> {
    for (_, sender) in lock_session(pending, "host calls")?.drain() {
        let _ = sender.send(HostResponse::Shutdown);
    }
    Ok(())
}

fn validate_session_or_close(
    actual: &str,
    expected: &str,
    pending: &Mutex<HashMap<String, SyncSender<HostResponse>>>,
    shutdown_requested: &AtomicBool,
) -> Result<(), SdkError> {
    if let Err(error) = validate_session(actual, expected) {
        shutdown_requested.store(true, Ordering::Release);
        close_pending_calls(pending)?;
        return Err(error);
    }
    Ok(())
}

fn write_body<W: Write>(
    writer: &mut W,
    codec: &FrameCodec,
    body: MessageBody,
) -> Result<(), SdkError> {
    codec.write_to(
        writer,
        &WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body,
        },
    )?;
    writer
        .flush()
        .map_err(|source| SdkError::OutputFlush { source })?;
    Ok(())
}

fn validate_session(actual: &str, expected: &str) -> Result<(), SdkError> {
    if actual != expected {
        return Err(SdkError::InvalidSession {
            actual: actual.into(),
            expected: expected.into(),
        });
    }
    Ok(())
}

fn unexpected(body: &MessageBody, expected: &'static str) -> SdkError {
    SdkError::UnexpectedMessage {
        actual: message_type(body),
        expected,
    }
}

fn message_type(body: &MessageBody) -> &'static str {
    match body {
        MessageBody::HostHello { .. } => "host.hello",
        MessageBody::PluginReady { .. } => "plugin.ready",
        MessageBody::HostInvoke { .. } => "host.invoke",
        MessageBody::PluginResult { .. } => "plugin.result",
        MessageBody::PluginHostCall { .. } => "plugin.host_call",
        MessageBody::HostHostResult { .. } => "host.host_result",
        MessageBody::HostCancel { .. } => "host.cancel",
        MessageBody::HostShutdown { .. } => "host.shutdown",
        MessageBody::PluginStopped { .. } => "plugin.stopped",
    }
}

fn validate_host_correlation(field: &str, actual: &str, expected: &str) -> Result<(), PluginError> {
    if actual != expected {
        return Err(PluginError::degraded_protocol(format!(
            "host result {field} is `{actual}`; expected `{expected}`"
        )));
    }
    Ok(())
}

fn protocol_as_plugin_error(error: SdkError) -> PluginError {
    PluginError::degraded_protocol(error.to_string())
}
