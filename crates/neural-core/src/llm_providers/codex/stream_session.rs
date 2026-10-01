use super::{
    CODEX_ERROR_PREFIX, CODEX_MARKER_PREFIX, CODEX_NO_SESSION, CODEX_SESSION_PREFIX,
    CodexStreamState, codex_stateless_args, decorate_hot_error, emit_codex_json_line, hot_closed,
    hot_shell_script, missing_pipe, missing_spawn, parse_codex_output, write_prompt_file,
};
use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::LlmRequest;
use crate::llm_providers::contract::LlmStreamEventSink;
use crate::process::{ProcessFailureDiagnostics, StreamControl};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(super) struct CodexStreamProcess {
    child: Child,
    lines: mpsc::Receiver<std::io::Result<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
    stderr_path: PathBuf,
    started_at: Instant,
    command_display: String,
    timeout: Duration,
}

impl CodexStreamProcess {
    pub(super) fn spawn(
        config: &LlmProviderConfig,
        request: &LlmRequest,
        output_path: &Path,
        stderr_path: &Path,
    ) -> Result<Self> {
        let spawn = config
            .spawn
            .as_ref()
            .ok_or_else(|| missing_spawn("codex"))?;
        let mut command = Command::new(&spawn.command);
        command.args(spawn.args.iter());
        command.args(codex_stateless_args(config, request, output_path));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(
            std::fs::File::create(stderr_path).map_err(|source| NeuralError::Io {
                value: stderr_path.display().to_string(),
                expected: "temporary Codex stderr file".to_string(),
                source,
            })?,
        );
        let mut child = command.spawn().map_err(|source| NeuralError::Io {
            value: spawn.display_command(),
            expected: "streaming Codex command process".to_string(),
            source,
        })?;
        let stdout = child.stdout.take().ok_or_else(|| missing_pipe("stdout"))?;
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || read_codex_stdout_lines(stdout, sender));
        Ok(Self {
            child,
            lines: receiver,
            reader: Some(reader),
            stderr_path: stderr_path.to_path_buf(),
            started_at: Instant::now(),
            command_display: spawn.display_command(),
            timeout: spawn.timeout(),
        })
    }

    pub(super) fn drain(
        &mut self,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<CodexStreamState> {
        let mut state = CodexStreamState::default();
        loop {
            self.kill_if_timed_out()?;
            self.drain_available_lines(control.clone(), &mut state, on_event)?;
            if self.exited()? {
                self.drain_available_lines(control.clone(), &mut state, on_event)?;
                self.detach_reader();
                self.ensure_success()?;
                return Ok(state);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn drain_available_lines(
        &mut self,
        control: StreamControl,
        state: &mut CodexStreamState,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        while let Ok(line) = self.lines.try_recv() {
            let line = line.map_err(|source| NeuralError::Io {
                value: self.command_display.clone(),
                expected: "streaming Codex stdout line".to_string(),
                source,
            })?;
            emit_codex_json_line(&line, control.clone(), state, on_event)?;
        }
        Ok(())
    }

    fn kill_if_timed_out(&mut self) -> Result<()> {
        if self.started_at.elapsed() <= self.timeout {
            return Ok(());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Err(NeuralError::ProcessTimeout {
            command: self.command_display.clone(),
            timeout_ms: self.timeout.as_millis() as u64,
        })
    }

    fn exited(&mut self) -> Result<bool> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|source| NeuralError::Io {
                value: self.command_display.clone(),
                expected: "streaming Codex command status".to_string(),
                source,
            })
    }

    fn ensure_success(&mut self) -> Result<()> {
        let status = self.child.wait().map_err(|source| NeuralError::Io {
            value: self.command_display.clone(),
            expected: "streaming Codex command completion".to_string(),
            source,
        })?;
        if status.success() {
            return Ok(());
        }
        Err(NeuralError::ProcessFailed {
            command: self.command_display.clone(),
            status: status.to_string(),
            diagnostics: ProcessFailureDiagnostics::from_stderr(
                std::fs::read_to_string(&self.stderr_path).unwrap_or_default(),
            ),
        })
    }

    fn detach_reader(&mut self) {
        let _ = self.reader.take();
    }
}

pub(super) fn read_codex_stdout_lines(
    stdout: ChildStdout,
    sender: mpsc::Sender<std::io::Result<String>>,
) {
    for line in BufReader::new(stdout).lines() {
        if sender.send(line).is_err() {
            return;
        }
    }
}

pub(super) struct CodexHotSession {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    sequence: u64,
    provider_session_id: Option<String>,
}

pub(super) struct CodexTurnOutput {
    pub(super) content: String,
    pub(super) provider_session_id: Option<String>,
}

impl CodexHotSession {
    pub(super) fn spawn(command: &str, config: &LlmProviderConfig) -> Result<Self> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(hot_shell_script(command, config))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| NeuralError::Io {
                value: command.to_string(),
                expected: "codex hot shell process".to_string(),
                source,
            })?;
        let stdin = child.stdin.take().ok_or_else(|| missing_pipe("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing_pipe("stdout"))?;
        Ok(Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            sequence: 0,
            provider_session_id: None,
        })
    }

    pub(super) fn run_turn(
        &mut self,
        prompt: &str,
        config: &LlmProviderConfig,
    ) -> Result<CodexTurnOutput> {
        let marker = self.next_marker();
        let prompt_path = write_prompt_file(prompt)?;
        self.write_request(&marker, &prompt_path)?;
        let raw_output = self.read_until_marker(&marker)?;
        parse_codex_output(&raw_output)
            .map(|content| CodexTurnOutput {
                content,
                provider_session_id: self.provider_session_id.clone(),
            })
            .map_err(|error| decorate_hot_error(config, error))
    }

    pub(super) fn set_provider_session_id(&mut self, provider_session_id: Option<String>) {
        if self.provider_session_id.is_some() || provider_session_id.is_none() {
            return;
        }
        self.provider_session_id = provider_session_id;
    }

    fn next_marker(&mut self) -> String {
        self.sequence = self.sequence.saturating_add(1);
        format!("{CODEX_MARKER_PREFIX}{}", self.sequence)
    }

    fn write_request(&mut self, marker: &str, prompt_path: &Path) -> Result<()> {
        let session = self
            .provider_session_id
            .as_deref()
            .unwrap_or(CODEX_NO_SESSION);
        let request = format!("{marker}\t{session}\t{}\n", prompt_path.display());
        self.stdin
            .write_all(request.as_bytes())
            .map_err(|source| NeuralError::Io {
                value: marker.to_string(),
                expected: "writable codex hot stdin".to_string(),
                source,
            })?;
        self.stdin.flush().map_err(|source| NeuralError::Io {
            value: marker.to_string(),
            expected: "flushable codex hot stdin".to_string(),
            source,
        })
    }

    fn read_until_marker(&mut self, marker: &str) -> Result<String> {
        let mut output = String::new();
        loop {
            let mut line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .map_err(|source| NeuralError::Io {
                    value: marker.to_string(),
                    expected: "codex hot stdout line".to_string(),
                    source,
                })?;
            if bytes == 0 {
                return Err(hot_closed(marker));
            }
            if self.consume_hot_line(marker, &line, &mut output)? {
                return Ok(output);
            }
        }
    }

    fn consume_hot_line(&mut self, marker: &str, line: &str, output: &mut String) -> Result<bool> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line == marker {
            return Ok(true);
        }
        if let Some(message) = line.strip_prefix(CODEX_ERROR_PREFIX) {
            return Err(NeuralError::ProcessFailed {
                command: "codex hot session".to_string(),
                status: "failed".to_string(),
                diagnostics: ProcessFailureDiagnostics::from_stdout(message.trim()),
            });
        }
        if let Some(session_id) = line.strip_prefix(CODEX_SESSION_PREFIX) {
            self.provider_session_id = Some(session_id.trim().to_string());
            return Ok(false);
        }
        output.push_str(line);
        output.push('\n');
        Ok(false)
    }
}
