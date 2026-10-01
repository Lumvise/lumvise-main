use crate::config::{LlmProviderConfig, SpawnConfig};
use crate::error::{NeuralError, Result};
use crate::llm_providers::contract::ProviderCallControl;
use crate::llm_providers::{LlmRequest, LlmResponse};
use crate::process::ProcessFailureDiagnostics;
use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct ProviderCommandRunner {
    spawn: SpawnConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCommandOutput {
    pub stdout: String,
    pub stderr: String,
}

pub type ProviderStdoutLineSink<'a> = dyn FnMut(String) -> Result<()> + 'a;
/// Injectable command boundary used for provider inventory and utilization
/// probes. Production calls keep the existing timeout and spawn policy.
pub trait LlmCommandTransport: Send + Sync {
    fn run(
        &self,
        spawn: &SpawnConfig,
        args: Vec<String>,
        stdin: Option<&str>,
    ) -> Result<ProviderCommandOutput>;
}

#[derive(Default)]
pub struct ProviderCommandTransport;

impl LlmCommandTransport for ProviderCommandTransport {
    fn run(
        &self,
        spawn: &SpawnConfig,
        args: Vec<String>,
        stdin: Option<&str>,
    ) -> Result<ProviderCommandOutput> {
        ProviderCommandRunner::new(spawn.clone())?.run(args, stdin)
    }
}

impl ProviderCommandRunner {
    pub fn new(spawn: SpawnConfig) -> Result<Self> {
        spawn.validate()?;
        Ok(Self { spawn })
    }

    pub fn run(&self, args: Vec<String>, stdin: Option<&str>) -> Result<ProviderCommandOutput> {
        self.run_with_cwd(args, stdin, None)
    }

    pub fn run_with_cwd(
        &self,
        args: Vec<String>,
        stdin: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<ProviderCommandOutput> {
        let mut command = Command::new(&self.spawn.command);
        command.args(self.spawn.args.iter().chain(args.iter()));
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let started_at = Instant::now();
        let mut child = spawn_child(command, &self.spawn)?;
        write_stdin(&mut child, stdin, &self.spawn)?;
        let stdout_reader = spawn_pipe_reader(take_stdout(&mut child, &self.spawn)?);
        let stderr_reader = spawn_pipe_reader(take_stderr(&mut child, &self.spawn)?);

        let status = match wait_with_timeout(&mut child, &self.spawn, started_at) {
            Ok(status) => status,
            Err(error) => {
                let _ = join_pipe_reader(stdout_reader, &self.spawn, "provider stdout");
                let _ = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr");
                return Err(error);
            }
        };
        let stdout = join_pipe_reader(stdout_reader, &self.spawn, "provider stdout")?;
        let stderr = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr")?;
        collect_output(status, stdout, stderr, &self.spawn)
    }

    /// Cancels the subprocess when `control` expires or is cancelled mid-call,
    /// instead of only enforcing `SpawnConfig.timeout_ms`. CLI providers'
    /// `complete_controlled` path uses this so a caller that gives up
    /// actually kills the underlying CLI process promptly.
    pub fn run_cancellable(
        &self,
        args: Vec<String>,
        stdin: Option<&str>,
        control: &dyn ProviderCallControl,
    ) -> Result<ProviderCommandOutput> {
        self.run_with_cwd_cancellable(args, stdin, None, control)
    }

    pub fn run_with_cwd_cancellable(
        &self,
        args: Vec<String>,
        stdin: Option<&str>,
        cwd: Option<&Path>,
        control: &dyn ProviderCallControl,
    ) -> Result<ProviderCommandOutput> {
        let mut command = Command::new(&self.spawn.command);
        command.args(self.spawn.args.iter().chain(args.iter()));
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let started_at = Instant::now();
        let mut child = spawn_child(command, &self.spawn)?;
        write_stdin(&mut child, stdin, &self.spawn)?;
        let stdout_reader = spawn_pipe_reader(take_stdout(&mut child, &self.spawn)?);
        let stderr_reader = spawn_pipe_reader(take_stderr(&mut child, &self.spawn)?);

        let status =
            match wait_with_timeout_controlled(&mut child, &self.spawn, started_at, control) {
                Ok(status) => status,
                Err(error) => {
                    let _ = join_pipe_reader(stdout_reader, &self.spawn, "provider stdout");
                    let _ = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr");
                    return Err(error);
                }
            };
        let stdout = join_pipe_reader(stdout_reader, &self.spawn, "provider stdout")?;
        let stderr = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr")?;
        collect_output(status, stdout, stderr, &self.spawn)
    }

    pub fn command(&self) -> &str {
        &self.spawn.command
    }

    pub fn run_stdout_lines(
        &self,
        args: Vec<String>,
        stdin: Option<&str>,
        on_line: &mut ProviderStdoutLineSink<'_>,
    ) -> Result<ProviderCommandOutput> {
        self.run_stdout_lines_with_cwd(args, stdin, None, on_line)
    }

    pub fn run_stdout_lines_with_cwd(
        &self,
        args: Vec<String>,
        stdin: Option<&str>,
        cwd: Option<&Path>,
        on_line: &mut ProviderStdoutLineSink<'_>,
    ) -> Result<ProviderCommandOutput> {
        let started_at = Instant::now();
        let mut child = self.spawn_stream_child(args, stdin, cwd)?;
        let stderr_reader = spawn_pipe_reader(take_stderr(&mut child, &self.spawn)?);
        let stdout = match read_child_stdout(&mut child, &self.spawn, started_at, on_line) {
            Ok(stdout) => stdout,
            Err(error) => {
                let _ = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr");
                return Err(error);
            }
        };

        let status = match wait_with_timeout(&mut child, &self.spawn, started_at) {
            Ok(status) => status,
            Err(error) => {
                let _ = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr");
                return Err(error);
            }
        };
        let stderr = join_pipe_reader(stderr_reader, &self.spawn, "provider stderr")?;
        collect_output(status, stdout.into_bytes(), stderr, &self.spawn)
    }

    fn spawn_stream_child(
        &self,
        args: Vec<String>,
        stdin: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<Child> {
        let mut command = Command::new(&self.spawn.command);
        command.args(self.spawn.args.iter().chain(args.iter()));
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = spawn_child(command, &self.spawn)?;
        write_stdin(&mut child, stdin, &self.spawn)?;
        Ok(child)
    }
}

pub(crate) fn cli_spawn(config: &LlmProviderConfig) -> Result<SpawnConfig> {
    config
        .spawn
        .clone()
        .ok_or_else(|| NeuralError::MissingValue {
            value: config.provider_id.clone(),
            expected: "CLI provider spawn config".to_string(),
        })
}

pub(crate) fn model_args(model: &str) -> Vec<String> {
    let model = model.trim();
    if model.is_empty() {
        return Vec::new();
    }
    vec!["--model".to_string(), model.to_string()]
}

pub(crate) fn selected_model(config: &LlmProviderConfig, request: &LlmRequest) -> String {
    request
        .model_id()
        .unwrap_or(config.model.as_str())
        .to_string()
}

pub(crate) fn prompt_text(request: &LlmRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| format!("{}: {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn replace_arg_value(args: &mut [String], flag: &str, value: &str) {
    if let Some(index) = args.iter().position(|arg| arg == flag) {
        args[index + 1] = value.to_string();
    }
}

pub(crate) fn response(
    config: &LlmProviderConfig,
    request: &LlmRequest,
    content: String,
    metadata: Value,
) -> LlmResponse {
    LlmResponse {
        provider_id: config.provider_id.clone(),
        model: selected_model(config, request),
        content,
        metadata,
    }
}

fn spawn_child(mut command: Command, spawn: &SpawnConfig) -> Result<std::process::Child> {
    command.spawn().map_err(|source| NeuralError::Io {
        value: spawn.display_command(),
        expected: "provider command process".to_string(),
        source,
    })
}

fn write_stdin(
    child: &mut std::process::Child,
    stdin: Option<&str>,
    spawn: &SpawnConfig,
) -> Result<()> {
    let Some(stdin) = stdin else {
        drop(child.stdin.take());
        return Ok(());
    };
    let mut pipe = child
        .stdin
        .take()
        .ok_or_else(|| NeuralError::MissingValue {
            value: "stdin".to_string(),
            expected: "provider command stdin".to_string(),
        })?;
    pipe.write_all(stdin.as_bytes())
        .map_err(|source| NeuralError::Io {
            value: spawn.display_command(),
            expected: "writable provider stdin".to_string(),
            source,
        })?;
    Ok(())
}

fn wait_with_timeout(
    child: &mut Child,
    spawn: &SpawnConfig,
    started_at: Instant,
) -> Result<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(source) => {
                terminate_child(child);
                return Err(NeuralError::Io {
                    value: spawn.display_command(),
                    expected: "provider command status".to_string(),
                    source,
                });
            }
        }

        let Some(remaining) = timeout_remaining(spawn, started_at) else {
            return Err(timeout_child(child, spawn));
        };
        thread::sleep(Duration::from_millis(5).min(remaining));
    }
}

/// Like `wait_with_timeout` but also terminates the child when the caller
/// cancels or its deadline expires. Cancellation is terminal for the session;
/// deadline expiry is a recoverable provider-turn timeout. Pipe draining is
/// preserved on every terminal path.
fn wait_with_timeout_controlled(
    child: &mut Child,
    spawn: &SpawnConfig,
    started_at: Instant,
    control: &dyn ProviderCallControl,
) -> Result<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(source) => {
                terminate_child(child);
                return Err(NeuralError::Io {
                    value: spawn.display_command(),
                    expected: "provider command status".to_string(),
                    source,
                });
            }
        }

        if control.is_cancelled() {
            return Err(cancel_child(child, spawn));
        }
        if control.is_expired() {
            return Err(timeout_child(child, spawn));
        }

        let Some(remaining) = timeout_remaining(spawn, started_at) else {
            return Err(timeout_child(child, spawn));
        };
        thread::sleep(Duration::from_millis(5).min(remaining));
    }
}

fn read_child_stdout(
    child: &mut Child,
    spawn: &SpawnConfig,
    started_at: Instant,
    on_line: &mut ProviderStdoutLineSink<'_>,
) -> Result<String> {
    let stdout = take_stdout(child, spawn)?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || -> std::io::Result<()> {
        for line in std::io::BufReader::new(stdout).lines() {
            if sender.send(line?).is_err() {
                return Ok(());
            }
        }
        Ok(())
    });
    let mut text = String::new();

    loop {
        let Some(remaining) = timeout_remaining(spawn, started_at) else {
            let error = timeout_child(child, spawn);
            let _ = join_stdout_line_reader(reader, spawn);
            return Err(error);
        };

        match receiver.recv_timeout(remaining) {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
                if let Err(error) = on_line(line) {
                    terminate_child(child);
                    let _ = join_stdout_line_reader(reader, spawn);
                    return Err(error);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let error = timeout_child(child, spawn);
                let _ = join_stdout_line_reader(reader, spawn);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Err(error) = join_stdout_line_reader(reader, spawn) {
                    terminate_child(child);
                    return Err(error);
                }
                return Ok(text);
            }
        }
    }
}

fn take_stdout(child: &mut Child, spawn: &SpawnConfig) -> Result<ChildStdout> {
    child
        .stdout
        .take()
        .ok_or_else(|| NeuralError::MissingValue {
            value: spawn.display_command(),
            expected: "provider command stdout".to_string(),
        })
}

fn take_stderr(child: &mut Child, spawn: &SpawnConfig) -> Result<ChildStderr> {
    child
        .stderr
        .take()
        .ok_or_else(|| NeuralError::MissingValue {
            value: spawn.display_command(),
            expected: "provider command stderr".to_string(),
        })
}

fn spawn_pipe_reader<R>(pipe: R) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = pipe;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_pipe_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    spawn: &SpawnConfig,
    expected: &str,
) -> Result<Vec<u8>> {
    match reader.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(source)) => Err(NeuralError::Io {
            value: spawn.display_command(),
            expected: expected.to_string(),
            source,
        }),
        Err(_) => Err(reader_thread_panic_error(spawn, expected)),
    }
}

fn join_stdout_line_reader(
    reader: thread::JoinHandle<std::io::Result<()>>,
    spawn: &SpawnConfig,
) -> Result<()> {
    match reader.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err(NeuralError::Io {
            value: spawn.display_command(),
            expected: "provider stdout line".to_string(),
            source,
        }),
        Err(_) => Err(reader_thread_panic_error(spawn, "provider stdout line")),
    }
}

fn reader_thread_panic_error(spawn: &SpawnConfig, expected: &str) -> NeuralError {
    NeuralError::Io {
        value: spawn.display_command(),
        expected: expected.to_string(),
        source: std::io::Error::new(
            std::io::ErrorKind::Other,
            "provider pipe reader thread panicked",
        ),
    }
}

fn timeout_remaining(spawn: &SpawnConfig, started_at: Instant) -> Option<Duration> {
    spawn.timeout().checked_sub(started_at.elapsed())
}

fn timeout_child(child: &mut Child, spawn: &SpawnConfig) -> NeuralError {
    terminate_child(child);
    NeuralError::ProcessTimeout {
        command: spawn.display_command(),
        timeout_ms: spawn.timeout_ms,
    }
}

fn cancel_child(child: &mut Child, spawn: &SpawnConfig) -> NeuralError {
    terminate_child(child);
    NeuralError::ProcessCancelled {
        command: spawn.display_command(),
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn collect_output(
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    spawn: &SpawnConfig,
) -> Result<ProviderCommandOutput> {
    if status.success() {
        return Ok(ProviderCommandOutput {
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        });
    }
    Err(NeuralError::ProcessFailed {
        command: spawn.display_command(),
        status: status.to_string(),
        diagnostics: ProcessFailureDiagnostics::from_streams(&stdout, &stderr),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LARGE_OUTPUT_BYTES: usize = 200_000;

    fn shell_runner(timeout_ms: u64) -> ProviderCommandRunner {
        ProviderCommandRunner::new(SpawnConfig {
            command: "sh".to_string(),
            args: Vec::new(),
            timeout_ms,
        })
        .expect("shell spawn config is valid")
    }

    fn run_shell(command: &str, timeout_ms: u64) -> Result<ProviderCommandOutput> {
        shell_runner(timeout_ms).run(vec!["-c".to_string(), command.to_string()], None)
    }

    #[test]
    fn drains_large_stdout_before_waiting_for_exit() {
        let started_at = Instant::now();
        let output = run_shell("yes | head -c 200000", 5_000).expect("large stdout succeeds");

        assert_eq!(output.stdout.len(), LARGE_OUTPUT_BYTES);
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "large stdout command exceeded the non-timeout threshold"
        );
    }

    #[test]
    fn drains_large_stderr_before_waiting_for_exit() {
        let started_at = Instant::now();
        let output = run_shell("yes | head -c 200000 >&2", 5_000).expect("large stderr succeeds");

        assert_eq!(output.stderr.len(), LARGE_OUTPUT_BYTES);
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "large stderr command exceeded the non-timeout threshold"
        );
    }

    #[test]
    fn streams_stdout_while_draining_large_stderr() {
        let mut lines = Vec::new();
        let started_at = Instant::now();
        let output = shell_runner(5_000)
            .run_stdout_lines(
                vec![
                    "-c".to_string(),
                    "printf 'first\\nsecond\\n'; yes | head -c 200000 >&2".to_string(),
                ],
                None,
                &mut |line| {
                    lines.push(line);
                    Ok(())
                },
            )
            .expect("streaming command succeeds");

        assert_eq!(lines, ["first", "second"]);
        assert_eq!(output.stdout, "first\nsecond\n");
        assert_eq!(output.stderr.len(), LARGE_OUTPUT_BYTES);
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "streaming command exceeded the non-timeout threshold"
        );
    }

    #[test]
    fn preserves_short_output_failures_and_timeouts() {
        let output =
            run_shell("printf stdout; printf stderr >&2", 5_000).expect("short command succeeds");
        assert_eq!(output.stdout, "stdout");
        assert_eq!(output.stderr, "stderr");

        let error =
            run_shell("printf failed >&2; exit 7", 5_000).expect_err("non-zero command fails");
        assert!(matches!(
            error,
            NeuralError::ProcessFailed {
                ref diagnostics,
                ..
            } if diagnostics.stderr() == "failed" && diagnostics.stdout().is_empty()
        ));

        let started_at = Instant::now();
        let error = run_shell("sleep 5", 50).expect_err("slow command times out");
        assert!(matches!(
            error,
            NeuralError::ProcessTimeout { timeout_ms: 50, .. }
        ));
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "timed out command was not killed promptly"
        );
    }

    /// Regression for #88: the configured engine CLI reports `Not logged in ·
    /// Please run /login` as JSON on stdout and writes nothing to stderr, so a
    /// stderr-only failure recorded an empty reason.
    #[test]
    fn records_the_reason_a_failing_process_reports_on_stdout() {
        let error = run_shell(
            "printf '{\"error\":\"Not logged in - Please run /login\"}'; exit 1",
            5_000,
        )
        .expect_err("non-zero command fails");

        assert!(
            error
                .to_string()
                .contains("Not logged in - Please run /login"),
            "failure must carry the reason the process reported: {error}"
        );
    }

    #[test]
    fn bounds_captured_output_so_a_chatty_failure_stays_readable() {
        let error = run_shell("yes chatter | head -c 200000; exit 1", 5_000)
            .expect_err("non-zero command fails");

        assert!(
            error.to_string().len() < 8_192,
            "captured output must be bounded, got {} bytes",
            error.to_string().len()
        );
    }
    #[test]
    fn expired_control_classifies_child_as_timeout() {
        struct ExpiredControl;
        impl ProviderCallControl for ExpiredControl {
            fn remaining(&self) -> Duration {
                Duration::ZERO
            }
            fn is_cancelled(&self) -> bool {
                false
            }
        }

        let started_at = Instant::now();
        let error = shell_runner(30_000)
            .run_cancellable(
                vec!["-c".to_string(), "sleep 30".to_string()],
                None,
                &ExpiredControl,
            )
            .expect_err("expired control terminates the subprocess");
        assert!(
            matches!(error, NeuralError::ProcessTimeout { .. }),
            "expected ProcessTimeout, got {error:?}"
        );
        assert!(
            started_at.elapsed() < Duration::from_secs(5),
            "expired command was not killed promptly (elapsed {:?})",
            started_at.elapsed()
        );
    }
}
