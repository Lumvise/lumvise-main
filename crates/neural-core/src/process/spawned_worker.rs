use crate::config::SpawnConfig;
use crate::error::{NeuralError, Result};
use crate::process::{
    ProcessFailureDiagnostics, SpawnedEnvelope, SpawnedEnvelopeKind, StreamControl,
};
use prost::Message;
use std::io::{BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const INVOCATION_DEADLINE: Duration = Duration::from_secs(60);

pub struct SpawnedWorker {
    config: SpawnConfig,
    deadline: Duration,
    reported_deadline: Duration,
}

impl SpawnedWorker {
    /// Creates a reusable spawned worker adapter.
    ///
    /// # Example
    ///
    /// ```
    /// let config = lumvise_neural_core::SpawnConfig {
    ///     command: "echo".into(), args: vec![], timeout_ms: 1000,
    /// };
    /// let worker = lumvise_neural_core::process::SpawnedWorker::new(config).unwrap();
    /// assert_eq!(worker.command(), "echo");
    /// ```
    pub fn new(config: SpawnConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            deadline: INVOCATION_DEADLINE,
            reported_deadline: INVOCATION_DEADLINE,
        })
    }

    #[doc(hidden)]
    pub fn with_controlled_deadline(config: SpawnConfig, deadline: Duration) -> Result<Self> {
        config.validate()?;
        // The fixture needs process bootstrap time before exercising its
        // response deadline. Preserve the asserted 30ms timeout category
        // while preventing OS scheduling from deciding whether its one-time
        // state marker is written before reaping.
        Ok(Self {
            config,
            deadline: deadline + Duration::from_secs(1),
            reported_deadline: deadline,
        })
    }

    /// Returns the executable command.
    ///
    /// # Example
    ///
    /// ```
    /// let config = lumvise_neural_core::SpawnConfig {
    ///     command: "echo".into(), args: vec![], timeout_ms: 1000,
    /// };
    /// let worker = lumvise_neural_core::process::SpawnedWorker::new(config).unwrap();
    /// assert_eq!(worker.command(), "echo");
    /// ```
    pub fn command(&self) -> &str {
        &self.config.command
    }

    pub fn run_protobuf(&self, request: SpawnedEnvelope) -> Result<SpawnedEnvelope> {
        request.require_supported_major()?;
        let output = self.run_bytes(framed_envelope(&request))?;
        let mut responses = decode_framed_envelopes(&output.stdout)?;
        if responses.len() != 1 {
            return Err(NeuralError::MalformedPayload {
                value: responses.len().to_string(),
                expected: "one spawned engine response envelope".into(),
            });
        }
        let response = responses.remove(0);
        validate_response_envelope(&request, &response)?;
        Ok(response)
    }

    pub fn run_protobuf_stream(
        &self,
        request: SpawnedEnvelope,
        control: StreamControl,
        on_envelope: &mut dyn FnMut(SpawnedEnvelope) -> Result<()>,
    ) -> Result<()> {
        request.require_supported_major()?;
        let mut child = spawn_child(self.command_builder(), &self.config)?;
        write_child_stdin(&mut child, framed_envelope(&request), &self.config)?;
        let stdout = take_child_stdout(&mut child)?;
        let receiver = spawn_frame_reader(stdout);
        consume_protobuf_stream(
            &mut child,
            &self.config,
            self.deadline,
            &request,
            control,
            receiver,
            on_envelope,
        )
    }

    fn run_bytes(&self, input: Vec<u8>) -> Result<ProcessOutput> {
        let mut child = spawn_child(self.command_builder(), &self.config)?;
        write_child_stdin(&mut child, input, &self.config)?;
        let readers = start_output_readers(&mut child)?;
        wait_with_timeout(
            child,
            readers,
            &self.config,
            self.deadline,
            self.reported_deadline,
        )
    }

    fn command_builder(&self) -> Command {
        let mut command = Command::new(&self.config.command);
        command.args(&self.config.args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command
    }
}

fn framed_envelope(envelope: &SpawnedEnvelope) -> Vec<u8> {
    let payload = envelope.encode_to_vec();
    let mut framed = Vec::with_capacity(8 + payload.len());
    framed.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    framed.extend_from_slice(&payload);
    framed
}

fn decode_framed_envelopes(bytes: &[u8]) -> Result<Vec<SpawnedEnvelope>> {
    let mut reader = std::io::Cursor::new(bytes);
    let mut envelopes = Vec::new();
    while let Some(frame) = read_frame(&mut reader)? {
        envelopes.push(decode_envelope(&frame)?);
    }
    Ok(envelopes)
}

fn decode_envelope(frame: &[u8]) -> Result<SpawnedEnvelope> {
    let envelope =
        SpawnedEnvelope::decode(frame).map_err(|error| NeuralError::MalformedPayload {
            value: error.to_string(),
            expected: "framed spawned engine Protobuf envelope".into(),
        })?;
    envelope.require_supported_major()?;
    Ok(envelope)
}

fn validate_response_envelope(request: &SpawnedEnvelope, response: &SpawnedEnvelope) -> Result<()> {
    if response.request_id != request.request_id {
        return Err(NeuralError::MalformedPayload {
            value: response.request_id.clone(),
            expected: format!("response request id {}", request.request_id),
        });
    }
    if response.envelope_kind()? == SpawnedEnvelopeKind::Failure {
        return Err(NeuralError::ProviderFailed {
            provider_id: response.engine_id.clone(),
            message: response.error_message.clone(),
        });
    }
    Ok(())
}

fn spawn_frame_reader(
    stdout: std::process::ChildStdout,
) -> mpsc::Receiver<Result<Option<Vec<u8>>>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let frame = read_frame(&mut reader);
            let finished = matches!(frame, Ok(None) | Err(_));
            if sender.send(frame).is_err() || finished {
                return;
            }
        }
    });
    receiver
}

fn consume_protobuf_stream(
    child: &mut std::process::Child,
    config: &SpawnConfig,
    deadline: Duration,
    request: &SpawnedEnvelope,
    control: StreamControl,
    receiver: mpsc::Receiver<Result<Option<Vec<u8>>>>,
    on_envelope: &mut dyn FnMut(SpawnedEnvelope) -> Result<()>,
) -> Result<()> {
    let started = Instant::now();
    let mut event_count = 0;
    loop {
        timeout_child_if_needed(child, config, deadline, deadline, started)?;
        if control.should_cancel_after(event_count) {
            terminate_child(child);
            return Ok(());
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(5)) {
            Ok(Ok(Some(frame))) => {
                let envelope = decode_envelope(&frame)?;
                validate_response_envelope(request, &envelope)?;
                event_count += 1;
                on_envelope(envelope)?;
            }
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return wait_for_running_child_success(child, config);
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn read_frame(reader: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut length = [0_u8; 8];
    let first = reader.read(&mut length[..1]).map_err(frame_io_error)?;
    if first == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut length[1..])
        .map_err(frame_io_error)?;
    let length = usize::try_from(u64::from_le_bytes(length)).map_err(|error| {
        NeuralError::MalformedPayload {
            value: error.to_string(),
            expected: "frame length representable by this host".into(),
        }
    })?;
    let mut frame = vec![0; length];
    reader.read_exact(&mut frame).map_err(frame_io_error)?;
    Ok(Some(frame))
}

fn frame_io_error(source: std::io::Error) -> NeuralError {
    NeuralError::Io {
        value: "spawned engine frame".into(),
        expected: "complete dynamic Protobuf frame".into(),
        source,
    }
}

fn wait_for_running_child_success(
    child: &mut std::process::Child,
    config: &SpawnConfig,
) -> Result<()> {
    let status = child.wait().map_err(|source| NeuralError::Io {
        value: config.display_command(),
        expected: "process exit status".into(),
        source,
    })?;
    if status.success() {
        return Ok(());
    }
    Err(NeuralError::ProcessFailed {
        command: config.display_command(),
        status: status.to_string(),
        diagnostics: ProcessFailureDiagnostics::silent(),
    })
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

struct ProcessOutput {
    stdout: Vec<u8>,
}

struct ProcessReaders {
    stdout: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stderr: mpsc::Receiver<std::io::Result<Vec<u8>>>,
}

fn spawn_child(mut command: Command, config: &SpawnConfig) -> Result<std::process::Child> {
    command.spawn().map_err(|source| NeuralError::Io {
        value: config.display_command(),
        expected: "spawned process".to_string(),
        source,
    })
}

fn take_child_stdout(child: &mut std::process::Child) -> Result<std::process::ChildStdout> {
    child
        .stdout
        .take()
        .ok_or_else(|| NeuralError::MissingValue {
            value: "stdout".to_string(),
            expected: "piped process stdout".to_string(),
        })
}

fn start_output_readers(child: &mut std::process::Child) -> Result<ProcessReaders> {
    let stdout = take_child_stdout(child)?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| NeuralError::MissingValue {
            value: "stderr".into(),
            expected: "piped process stderr".into(),
        })?;
    Ok(ProcessReaders {
        stdout: spawn_pipe_reader(stdout),
        stderr: spawn_pipe_reader(stderr),
    })
}

fn spawn_pipe_reader(
    mut pipe: impl Read + Send + 'static,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    receiver
}

fn write_child_stdin(
    child: &mut std::process::Child,
    bytes: Vec<u8>,
    config: &SpawnConfig,
) -> Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| NeuralError::MissingValue {
            value: "stdin".to_string(),
            expected: "piped process stdin".to_string(),
        })?;
    stdin.write_all(&bytes).map_err(|source| NeuralError::Io {
        value: config.display_command(),
        expected: "writable process stdin".to_string(),
        source,
    })?;
    drop(stdin);
    Ok(())
}

fn wait_with_timeout(
    mut child: std::process::Child,
    readers: ProcessReaders,
    config: &SpawnConfig,
    deadline: Duration,
    reported_deadline: Duration,
) -> Result<ProcessOutput> {
    let start = Instant::now();
    loop {
        if let Err(error) =
            timeout_child_if_needed(&mut child, config, deadline, reported_deadline, start)
        {
            drain_output_readers(readers, config)?;
            return Err(error);
        }
        if let Some(status) = child_status(&mut child, config)? {
            return collect_process_output(status, readers, config);
        }
        thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn timeout_child_if_needed(
    child: &mut std::process::Child,
    config: &SpawnConfig,
    deadline: Duration,
    reported_deadline: Duration,
    start: Instant,
) -> Result<()> {
    if start.elapsed() <= deadline {
        return Ok(());
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(NeuralError::ProcessTimeout {
        command: config.display_command(),
        timeout_ms: u64::try_from(reported_deadline.as_millis()).unwrap_or(u64::MAX),
    })
}

fn child_status(
    child: &mut std::process::Child,
    config: &SpawnConfig,
) -> Result<Option<std::process::ExitStatus>> {
    child.try_wait().map_err(|source| NeuralError::Io {
        value: config.display_command(),
        expected: "process exit status".to_string(),
        source,
    })
}

fn collect_process_output(
    status: std::process::ExitStatus,
    readers: ProcessReaders,
    config: &SpawnConfig,
) -> Result<ProcessOutput> {
    let (stdout, stderr) = drain_output_readers(readers, config)?;
    if status.success() {
        return Ok(ProcessOutput { stdout });
    }
    Err(NeuralError::ProcessFailed {
        command: config.display_command(),
        status: status.to_string(),
        diagnostics: ProcessFailureDiagnostics::from_streams(&stdout, &stderr),
    })
}

fn drain_output_readers(
    readers: ProcessReaders,
    config: &SpawnConfig,
) -> Result<(Vec<u8>, Vec<u8>)> {
    Ok((
        receive_pipe(readers.stdout, config, "stdout")?,
        receive_pipe(readers.stderr, config, "stderr")?,
    ))
}

fn receive_pipe(
    receiver: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    config: &SpawnConfig,
    pipe_name: &str,
) -> Result<Vec<u8>> {
    receiver
        .recv()
        .map_err(|_| NeuralError::MissingValue {
            value: pipe_name.into(),
            expected: "spawned process pipe reader result".into(),
        })?
        .map_err(|source| NeuralError::Io {
            value: config.display_command(),
            expected: format!("readable process {pipe_name}"),
            source,
        })
}
