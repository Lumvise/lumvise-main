use std::{
    collections::{HashMap, HashSet},
    io::{self, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginInvocationContext,
    PluginRuntimeConfig, PluginRuntimeError, system::PluginSystem,
};
use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, ProtocolError, WireMessage, WireOutcome,
};
use serde_json::Value;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    oneshot,
};

const MAX_STDERR_BYTES: usize = 64 * 1024;
const EXIT_STATUS_POLL: Duration = Duration::from_millis(5);
const EXIT_STATUS_TIMEOUT: Duration = Duration::from_millis(500);

pub(crate) struct PluginProcess {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    in_flight: Arc<Mutex<InFlightMap>>,
    control: Mutex<mpsc::Receiver<ControlEvent>>,
    stderr: Arc<Mutex<String>>,
    reader_thread: Option<JoinHandle<()>>,
    session_id: String,
}
pub(crate) struct InvocationAccess<'runtime> {
    pub(crate) declared_host_capabilities: &'runtime HashMap<String, String>,
    pub(crate) broker: Arc<dyn HostCapabilityBroker>,
    pub(crate) plugin_system: &'runtime PluginSystem,
    pub(crate) context: &'runtime PluginInvocationContext,
}

/// Per-invocation async responder used by the fixed reader thread.
///
/// The receiver is wakeable and can carry host calls followed by a terminal
/// result without occupying a Tokio worker while stdout is idle.
type Responder = UnboundedSender<Result<WireMessage, InvocationFailure>>;
type InFlightMap = HashMap<String, Responder>;

/// Cloneable failure routed by the reader thread so every in-flight waiter
/// rebuilds the terminal error with its own plugin identity and diagnostics.
#[derive(Clone)]
enum InvocationFailure {
    /// Plugin stdout failed or closed (including clean EOF after process exit).
    Transport {
        message: String,
        exit_status: Option<String>,
        stderr: String,
    },
    /// A routed message carried another process session.
    SessionMismatch { expected: String, actual: String },
}

impl InvocationFailure {
    fn into_runtime_error(self, plugin_id: &str) -> PluginRuntimeError {
        match self {
            Self::Transport {
                exit_status: Some(status),
                stderr,
                ..
            } => PluginRuntimeError::ProcessExited {
                plugin_id: plugin_id.to_owned(),
                status,
                stderr,
            },
            Self::Transport {
                message, stderr, ..
            } => PluginRuntimeError::Protocol {
                plugin_id: plugin_id.to_owned(),
                message,
                stderr,
            },
            Self::SessionMismatch { expected, actual } => {
                PluginRuntimeError::MessageSessionMismatch {
                    plugin_id: plugin_id.to_owned(),
                    expected,
                    actual,
                }
            }
        }
    }
}

/// Events that affect the whole process rather than one invocation.
enum ControlEvent {
    Ready(WireMessage),
    Stopped,
    ReadError(PluginRuntimeError),
}

#[derive(Debug)]
enum FrameWriteError {
    Encode(ProtocolError),
    Poisoned,
    Io(io::Error),
}

fn encode_then_write<W: Write>(
    writer: &Mutex<W>,
    message: &WireMessage,
) -> Result<(), FrameWriteError> {
    let frame = FrameCodec::default()
        .encode(message)
        .map_err(FrameWriteError::Encode)?;
    let mut writer = writer.lock().map_err(|_| FrameWriteError::Poisoned)?;
    writer.write_all(&frame).map_err(FrameWriteError::Io)
}

impl PluginProcess {
    pub(crate) fn spawn(
        mut command: Command,
        executable: &Path,
        plugin_id: &str,
        session_id: String,
    ) -> Result<Self, PluginRuntimeError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|source| PluginRuntimeError::Spawn {
                path: executable.to_path_buf(),
                source,
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| spawn_pipe_error(executable, "stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| spawn_pipe_error(executable, "stdout"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| spawn_pipe_error(executable, "stderr"))?;
        let child = Arc::new(Mutex::new(child));
        let stdin = Arc::new(Mutex::new(stdin));
        let in_flight: Arc<Mutex<InFlightMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (control_tx, control_rx) = mpsc::channel();
        let (stderr, stderr_thread) = spawn_stderr_reader(stderr_pipe);
        let reader_thread = spawn_reader(
            stdout,
            ReaderContext {
                child: Arc::clone(&child),
                stderr: Arc::clone(&stderr),
                stderr_thread,
                in_flight: Arc::clone(&in_flight),
                control: control_tx,
                plugin_id: plugin_id.to_owned(),
                session_id: session_id.clone(),
            },
        );
        Ok(Self {
            child,
            stdin,
            in_flight,
            control: Mutex::new(control_rx),
            stderr,
            reader_thread: Some(reader_thread),
            session_id,
        })
    }

    pub(crate) fn receive_handshake(
        &mut self,
        plugin_id: &str,
        deadline: Duration,
    ) -> Result<WireMessage, PluginRuntimeError> {
        match self.control_event(deadline) {
            Some(ControlEvent::Ready(message)) => Ok(message),
            Some(ControlEvent::ReadError(error)) => Err(error),
            Some(ControlEvent::Stopped) => Err(self.protocol_error(
                plugin_id,
                "plugin stopped before ready handshake".to_owned(),
            )),
            None => Err(PluginRuntimeError::HandshakeTimeout {
                plugin_id: plugin_id.to_owned(),
                deadline,
                stderr: self.diagnostics(),
            }),
        }
    }

    /// Waits for one process-wide control event, returning `None` on timeout,
    /// channel closure, or a poisoned control lock.
    fn control_event(&self, deadline: Duration) -> Option<ControlEvent> {
        let control = self.control.lock().ok()?;
        control.recv_timeout(deadline).ok()
    }
    pub(crate) fn send(
        &self,
        message: &WireMessage,
        plugin_id: &str,
    ) -> Result<(), PluginRuntimeError> {
        match encode_then_write(self.stdin.as_ref(), message) {
            Ok(()) => Ok(()),
            Err(FrameWriteError::Encode(error)) => {
                Err(self.protocol_error(plugin_id, error.to_string()))
            }
            Err(FrameWriteError::Io(error)) => {
                Err(self.protocol_error(plugin_id, error.to_string()))
            }
            Err(FrameWriteError::Poisoned) => Err(PluginRuntimeError::Protocol {
                plugin_id: plugin_id.to_owned(),
                message: "stdin mutex poisoned".to_owned(),
                stderr: self.diagnostics(),
            }),
        }
    }

    pub(crate) async fn invoke_async(
        &self,
        plugin_id: &str,
        invocation_id: &str,
        capability_id: &str,
        input: Value,
        config: &PluginRuntimeConfig,
        access: InvocationAccess<'_>,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let request = WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body: MessageBody::HostInvoke {
                session_id: self.session_id.clone(),
                invocation_id: invocation_id.to_owned(),
                capability_id: capability_id.to_owned(),
                input,
            },
        };
        let (responder, receiver) = unbounded_channel();
        self.register_responder(invocation_id, responder)?;
        let outcome = match self.send(&request, plugin_id) {
            Ok(()) => {
                self.wait_for_invocation_result_async(
                    plugin_id,
                    invocation_id,
                    receiver,
                    config,
                    access,
                )
                .await
            }
            Err(error) => Err(error),
        };
        self.remove_responder(invocation_id);
        outcome
    }

    pub(crate) fn stop(
        &self,
        plugin_id: &str,
        config: &PluginRuntimeConfig,
    ) -> Result<(), PluginRuntimeError> {
        let shutdown = WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body: MessageBody::HostShutdown {
                session_id: self.session_id.clone(),
                reason: Some("host stop requested".to_owned()),
            },
        };
        let _ = self.send(&shutdown, plugin_id);
        let graceful = matches!(
            self.control_event(config.shutdown_grace),
            Some(ControlEvent::Stopped)
        );
        if !graceful {
            let _ = self.child.lock().map(|mut child| child.kill());
        }
        let _ = self.child.lock().map(|mut child| child.wait());
        Ok(())
    }

    pub(crate) fn terminate(&self) {
        let _ = self.child.lock().map(|mut child| child.kill());
        let _ = self.child.lock().map(|mut child| child.wait());
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn process_id(&self) -> u32 {
        self.child
            .lock()
            .map(|child| child.id())
            .unwrap_or_default()
    }

    pub(crate) fn diagnostics(&self) -> String {
        self.stderr
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| "stderr diagnostics unavailable: buffer lock poisoned".to_owned())
    }

    fn register_responder(
        &self,
        invocation_id: &str,
        responder: Responder,
    ) -> Result<(), PluginRuntimeError> {
        let mut in_flight = self
            .in_flight
            .lock()
            .map_err(|_| PluginRuntimeError::InvocationAdmissionPoisoned)?;
        in_flight.insert(invocation_id.to_owned(), responder);
        Ok(())
    }

    fn remove_responder(&self, invocation_id: &str) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            in_flight.remove(invocation_id);
        }
    }

    async fn wait_for_invocation_result_async(
        &self,
        plugin_id: &str,
        expected_id: &str,
        mut receiver: UnboundedReceiver<Result<WireMessage, InvocationFailure>>,
        config: &PluginRuntimeConfig,
        access: InvocationAccess<'_>,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let deadline = access
            .context
            .effective_deadline(config.invocation_deadline());
        let cancellation = access.context.cancellation();
        let mut call_ids = HashSet::new();
        loop {
            if cancellation.is_cancelled() {
                return self.cancel_invocation(
                    plugin_id,
                    expected_id,
                    config.shutdown_grace,
                    access.context,
                );
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let error =
                    self.invocation_timeout(plugin_id, expected_id, config.invocation_deadline());
                let _ = self.cancel_invocation(
                    plugin_id,
                    expected_id,
                    config.shutdown_grace,
                    access.context,
                );
                return Err(error);
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => continue,
                _ = tokio::time::sleep(remaining) => continue,
                message = receiver.recv() => match message {
                    Some(Ok(message)) => match message.body {
                        MessageBody::PluginHostCall {
                            session_id,
                            invocation_id,
                            call_id,
                            capability_id,
                            input,
                        } => {
                            self.service_host_call_async(
                                plugin_id,
                                expected_id,
                                session_id,
                                invocation_id,
                                call_id,
                                capability_id,
                                input,
                                &mut call_ids,
                                access.declared_host_capabilities,
                                Arc::clone(&access.broker),
                                access.plugin_system,
                                access.context,
                                deadline,
                            )
                            .await?;
                        }
                        body => return self.terminal_invocation(plugin_id, expected_id, body),
                    },
                    Some(Err(failure)) => return Err(failure.into_runtime_error(plugin_id)),
                    None => return Err(self.protocol_error(
                        plugin_id,
                        "plugin stdout closed while invocation was in flight".to_owned(),
                    )),
                }
            }
        }
    }

    fn cancel_invocation(
        &self,
        plugin_id: &str,
        invocation_id: &str,
        grace: Duration,
        context: &PluginInvocationContext,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        let cancellation = WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body: MessageBody::HostCancel {
                session_id: self.session_id.clone(),
                invocation_id: invocation_id.to_owned(),
            },
        };
        self.send(&cancellation, plugin_id)?;
        // Wait briefly for any final message, then terminate the process.
        // Cancellation is process-wide: after this grace period, terminate the
        // process so every in-flight invocation in that process also ends.
        let _ = self.control_event(grace);
        self.terminate();
        Err(PluginRuntimeError::InvocationCancelled {
            plugin_id: plugin_id.to_owned(),
            request_id: context.request_id().to_owned(),
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the process boundary validates each wire correlation field explicitly"
    )]
    async fn service_host_call_async(
        &self,
        plugin_id: &str,
        parent_invocation_id: &str,
        session_id: String,
        invocation_id: String,
        call_id: String,
        capability_id: String,
        input: Value,
        call_ids: &mut HashSet<String>,
        declared: &HashMap<String, String>,
        broker: Arc<dyn HostCapabilityBroker>,
        plugin_system: &PluginSystem,
        context: &PluginInvocationContext,
        deadline: Instant,
    ) -> Result<(), PluginRuntimeError> {
        self.validate_host_call(
            plugin_id,
            parent_invocation_id,
            &session_id,
            &invocation_id,
            &call_id,
            call_ids,
        )?;
        let outcome = host_call_outcome_async(
            HostCall {
                plugin_id,
                invocation_id: parent_invocation_id,
                call_id: &call_id,
                capability_id: &capability_id,
                input,
            },
            declared,
            broker,
            plugin_system,
            context,
            deadline,
        )
        .await;
        if context.cancellation().is_cancelled() || Instant::now() >= deadline {
            return Ok(());
        }
        let result = WireMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            body: MessageBody::HostHostResult {
                session_id: self.session_id.clone(),
                call_id,
                outcome,
            },
        };
        self.send(&result, plugin_id)
    }

    fn validate_host_call(
        &self,
        plugin_id: &str,
        parent_invocation_id: &str,
        session_id: &str,
        invocation_id: &str,
        call_id: &str,
        call_ids: &mut HashSet<String>,
    ) -> Result<(), PluginRuntimeError> {
        if session_id != self.session_id {
            return Err(PluginRuntimeError::MessageSessionMismatch {
                plugin_id: plugin_id.to_owned(),
                expected: self.session_id.clone(),
                actual: session_id.to_owned(),
            });
        }
        if invocation_id != parent_invocation_id {
            return Err(PluginRuntimeError::HostCallParentMismatch {
                plugin_id: plugin_id.to_owned(),
                expected: parent_invocation_id.to_owned(),
                actual: invocation_id.to_owned(),
            });
        }
        if !call_ids.insert(call_id.to_owned()) {
            return Err(PluginRuntimeError::ReentrantHostCall {
                plugin_id: plugin_id.to_owned(),
                call_id: call_id.to_owned(),
            });
        }
        Ok(())
    }

    fn terminal_invocation(
        &self,
        plugin_id: &str,
        expected_id: &str,
        body: MessageBody,
    ) -> Result<WireOutcome, PluginRuntimeError> {
        match body {
            MessageBody::PluginResult { session_id, .. } if session_id != self.session_id => {
                Err(PluginRuntimeError::MessageSessionMismatch {
                    plugin_id: plugin_id.to_owned(),
                    expected: self.session_id.clone(),
                    actual: session_id,
                })
            }
            MessageBody::PluginResult {
                invocation_id,
                outcome,
                ..
            } if invocation_id == expected_id => Ok(outcome),
            MessageBody::PluginResult { invocation_id, .. } => {
                Err(PluginRuntimeError::CorrelationMismatch {
                    plugin_id: plugin_id.to_owned(),
                    expected: expected_id.to_owned(),
                    actual: invocation_id,
                })
            }
            other => Err(self.protocol_error(
                plugin_id,
                format!("unexpected invocation message `{other:?}`"),
            )),
        }
    }

    fn invocation_timeout(
        &self,
        plugin_id: &str,
        invocation_id: &str,
        deadline: Duration,
    ) -> PluginRuntimeError {
        PluginRuntimeError::InvocationTimeout {
            plugin_id: plugin_id.to_owned(),
            invocation_id: invocation_id.to_owned(),
            deadline,
            stderr: self.diagnostics(),
        }
    }

    fn protocol_error(&self, plugin_id: &str, message: String) -> PluginRuntimeError {
        PluginRuntimeError::Protocol {
            plugin_id: plugin_id.to_owned(),
            message,
            stderr: self.diagnostics(),
        }
    }

    fn join_readers(&mut self) {
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
    }
}

struct HostCall<'call> {
    plugin_id: &'call str,
    invocation_id: &'call str,
    call_id: &'call str,
    capability_id: &'call str,
    input: Value,
}

async fn host_call_outcome_async(
    call: HostCall<'_>,
    declared: &HashMap<String, String>,
    broker: Arc<dyn HostCapabilityBroker>,
    plugin_system: &PluginSystem,
    context: &PluginInvocationContext,
    deadline: Instant,
) -> WireOutcome {
    let Some(required_version) = declared.get(call.capability_id) else {
        return WireOutcome::Failed {
            error: undeclared_host_capability(call.capability_id).into_wire_error(),
        };
    };
    let request = HostCapabilityRequest {
        plugin_id: call.plugin_id.to_owned(),
        invocation_id: call.invocation_id.to_owned(),
        call_id: call.call_id.to_owned(),
        capability_id: call.capability_id.to_owned(),
        required_version: required_version.clone(),
        input: call.input.clone(),
    };
    let result = match invoke_host_capability_async(broker, request, context, deadline).await {
        Ok(value) if call.capability_id == "plugin.invoke" => {
            Box::pin(plugin_system.invoke_plugin_async(call.plugin_id, call.input, context)).await
        }
        Ok(value) => Ok(value),
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => WireOutcome::Succeeded { value },
        Err(error) => WireOutcome::Failed {
            error: error.into_wire_error(),
        },
    }
}

async fn invoke_host_capability_async(
    broker: Arc<dyn HostCapabilityBroker>,
    request: HostCapabilityRequest,
    context: &PluginInvocationContext,
    deadline: Instant,
) -> Result<Value, HostCapabilityError> {
    let capability_id = request.capability_id.clone();
    let invocation_context = context.clone();
    let cancellation = context.cancellation();
    let (sender, receiver) = oneshot::channel();
    thread::spawn(move || {
        let _ = sender.send(broker.invoke_controlled(request, &invocation_context));
    });
    tokio::select! {
        _ = cancellation.cancelled() => Err(host_call_cancelled(&capability_id)),
        _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) => {
            Err(host_call_timed_out(&capability_id))
        }
        result = receiver => result.unwrap_or_else(|_| Err(host_call_executor_failed(&capability_id))),
    }
}

fn host_call_cancelled(capability_id: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        "host_capability_cancelled",
        "parent Plugin Invocation was cancelled",
        false,
    )
}

fn host_call_timed_out(capability_id: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        "host_capability_deadline_exceeded",
        "parent Plugin Invocation deadline elapsed",
        true,
    )
}

fn host_call_executor_failed(capability_id: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        "host_capability_executor_failed",
        "Host Capability executor ended without a result",
        true,
    )
}

fn undeclared_host_capability(capability_id: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        "host_capability_undeclared",
        "plugin package did not request this Host Capability",
        false,
    )
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.child.lock().map(|mut child| child.kill());
        let _ = self.child.lock().map(|mut child| child.wait());
        self.join_readers();
    }
}

fn spawn_pipe_error(path: &Path, pipe: &'static str) -> PluginRuntimeError {
    PluginRuntimeError::Spawn {
        path: path.to_path_buf(),
        source: std::io::Error::other(format!("spawned plugin omitted piped {pipe}")),
    }
}

struct ReaderContext {
    child: Arc<Mutex<Child>>,
    stderr: Arc<Mutex<String>>,
    stderr_thread: JoinHandle<()>,
    in_flight: Arc<Mutex<InFlightMap>>,
    control: mpsc::Sender<ControlEvent>,
    plugin_id: String,
    session_id: String,
}

fn spawn_reader(stdout: impl Read + Send + 'static, context: ReaderContext) -> JoinHandle<()> {
    thread::spawn(move || {
        let ReaderContext {
            child,
            stderr,
            stderr_thread,
            in_flight,
            control,
            plugin_id,
            session_id,
        } = context;
        let codec = FrameCodec::default();
        let mut reader = BufReader::new(stdout);
        let mut stderr_thread = Some(stderr_thread);
        loop {
            let message = codec.read_from(&mut reader);
            if let Err(error) = &message {
                // Any read failure — codec error or clean EOF after process
                // exit — terminates every in-flight invocation.
                let failure =
                    transport_failure(&child, &stderr, &mut stderr_thread, error.to_string());
                broadcast_error(&in_flight, &plugin_id, failure.clone());
                let _ = control.send(ControlEvent::ReadError(
                    failure.into_runtime_error(&plugin_id),
                ));
                return;
            }
            let message = message.unwrap();
            if let MessageBody::PluginReady {
                session_id: ready_session,
                ..
            } = &message.body
                && ready_session == &session_id
            {
                let _ = control.send(ControlEvent::Ready(message));
                continue;
            }
            if let MessageBody::PluginStopped {
                session_id: stopped_session,
                ..
            } = &message.body
                && stopped_session == &session_id
            {
                // Graceful stop with in-flight invocations fails those
                // waiters instead of letting them hang until deadline.
                broadcast_error(
                    &in_flight,
                    &plugin_id,
                    transport_failure(
                        &child,
                        &stderr,
                        &mut stderr_thread,
                        "plugin stopped while invocations were in flight".to_owned(),
                    ),
                );
                let _ = control.send(ControlEvent::Stopped);
                return;
            }
            route_message(message, &in_flight, &plugin_id, &session_id);
        }
    })
}

/// Builds the terminal transport failure, capturing the child exit status and
/// joining the stderr thread so diagnostics are complete when the child died.
fn transport_failure(
    child: &Arc<Mutex<Child>>,
    stderr: &Arc<Mutex<String>>,
    stderr_thread: &mut Option<JoinHandle<()>>,
    message: String,
) -> InvocationFailure {
    let exit_status = wait_for_exit_status(child);
    if exit_status.is_some()
        && let Some(thread) = stderr_thread.take()
    {
        let _ = thread.join();
    }
    let stderr = stderr
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "stderr diagnostics unavailable: buffer lock poisoned".to_owned());
    InvocationFailure::Transport {
        message,
        exit_status,
        stderr,
    }
}

fn wait_for_exit_status(child: &Arc<Mutex<Child>>) -> Option<String> {
    let deadline = Instant::now() + EXIT_STATUS_TIMEOUT;
    loop {
        if let Ok(mut child) = child.lock() {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status.to_string()),
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(EXIT_STATUS_POLL);
    }
}

fn route_message(
    message: WireMessage,
    in_flight: &Arc<Mutex<InFlightMap>>,
    plugin_id: &str,
    session_id: &str,
) {
    let key = match &message.body {
        MessageBody::PluginResult { invocation_id, .. } => Some(invocation_id.clone()),
        MessageBody::PluginHostCall { invocation_id, .. } => Some(invocation_id.clone()),
        _ => None,
    };
    let Some(key) = key else {
        tracing::warn!(
            target: "lumvise-plugin-runtime::process",
            event = "plugin_message_unroutable",
            plugin = plugin_id,
            message = ?message.body,
            "dropping plugin message without an invocation correlation"
        );
        return;
    };
    let responder = in_flight.lock().ok().and_then(|map| {
        map.get(&key).cloned().or_else(|| {
            // With one in-flight invocation, route an unmatched correlation to
            // its waiter so strict wire validation reports the mismatch. With
            // multiple invocations, no waiter can be attributed safely.
            (map.len() == 1)
                .then(|| map.values().next().cloned())
                .flatten()
        })
    });
    let Some(responder) = responder else {
        tracing::warn!(
            target: "lumvise-plugin-runtime::process",
            event = "plugin_message_unmatched",
            plugin = plugin_id,
            invocation_id = key.as_str(),
            message = ?message.body,
            "dropping plugin message without a registered in-flight invocation"
        );
        return;
    };
    let message_session = message.body.session_id();
    let to_send = if message_session == session_id {
        Ok(message)
    } else {
        Err(InvocationFailure::SessionMismatch {
            expected: session_id.to_owned(),
            actual: message_session.to_owned(),
        })
    };
    let _ = responder.send(to_send);
}

fn broadcast_error(
    in_flight: &Arc<Mutex<InFlightMap>>,
    plugin_id: &str,
    failure: InvocationFailure,
) {
    let responders: Vec<(String, Responder)> = in_flight
        .lock()
        .map(|map| {
            map.iter()
                .map(|(key, responder)| (key.clone(), responder.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (invocation_id, responder) in responders {
        tracing::warn!(
            target: "lumvise-plugin-runtime::process",
            event = "plugin_invocation_transport_failed",
            plugin = plugin_id,
            invocation_id = invocation_id.as_str(),
            "failing in-flight invocation after plugin transport failure"
        );
        let _ = responder.send(Err(failure.clone()));
    }
}

fn spawn_stderr_reader(
    mut stderr_pipe: impl Read + Send + 'static,
) -> (Arc<Mutex<String>>, JoinHandle<()>) {
    let stderr = Arc::new(Mutex::new(String::new()));
    let output = Arc::clone(&stderr);
    let thread = thread::spawn(move || {
        let bytes = read_diagnostics(&mut stderr_pipe);
        if let Ok(mut diagnostic) = output.lock() {
            *diagnostic = String::from_utf8_lossy(&bytes).into_owned();
        }
    });
    (stderr, thread)
}

fn read_diagnostics(stderr: &mut impl Read) -> Vec<u8> {
    let mut captured = Vec::with_capacity(MAX_STDERR_BYTES);
    let mut chunk = [0_u8; 4096];
    while let Ok(count) = stderr.read(&mut chunk) {
        if count == 0 {
            break;
        }
        let remaining = MAX_STDERR_BYTES.saturating_sub(captured.len());
        captured.extend_from_slice(&chunk[..count.min(remaining)]);
    }
    captured
}

#[cfg(test)]
mod tests;
