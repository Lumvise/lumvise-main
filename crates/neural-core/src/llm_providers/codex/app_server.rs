//! Private Codex app-server adapter. Persistent turns use the external provider's
//! JSON-RPC API; Lumvise compiled-plugin transport remains Protobuf.
use super::persistent::CodexInvocation;
use super::{
    CodexStreamState, CodexTurnOutput, codex_app_server_process_args, codex_mcp_server_urls,
    emit_stream_events, isolated_codex_home, missing_pipe, missing_spawn, selected_codex_model,
    unique_output_path,
};
use crate::config::LlmProviderConfig;
use crate::error::{NeuralError, Result};
use crate::llm_providers::command_runner::prompt_text;
use crate::llm_providers::contract::LlmStreamEventSink;
use crate::llm_providers::{LlmRequest, LlmStreamEvent};
use crate::process::ProcessFailureDiagnostics;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub(super) struct CodexAppServerSession {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<std::io::Result<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
    codex_home: Option<PathBuf>,
    stderr_path: PathBuf,
    command_display: String,
    model: Option<String>,
    mcp_server_urls: Vec<String>,
    next_id: u64,
    provider_thread_id: Option<String>,
}

impl CodexAppServerSession {
    pub(super) fn spawn(
        config: &LlmProviderConfig,
        request: &LlmRequest,
        invocation: &CodexInvocation<'_>,
    ) -> Result<Self> {
        let spawn = config
            .spawn
            .as_ref()
            .ok_or_else(|| missing_spawn("codex"))?;
        let mut command = Command::new(&spawn.command);
        let codex_home = isolated_codex_home(request)?;
        if let Some(codex_home) = &codex_home {
            command.env("CODEX_HOME", codex_home);
        }
        let stderr_path = unique_output_path("codex-app-server-stderr");
        command.args(spawn.args.iter());
        command.args(codex_app_server_process_args(config, request));
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(
            std::fs::File::create(&stderr_path).map_err(|source| NeuralError::Io {
                value: stderr_path.display().to_string(),
                expected: "temporary Codex app-server stderr file".to_string(),
                source,
            })?,
        );
        let mut child = command.spawn().map_err(|source| NeuralError::Io {
            value: spawn.display_command(),
            expected: "persistent Codex app-server process".to_string(),
            source,
        })?;
        let stdin = child.stdin.take().ok_or_else(|| missing_pipe("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing_pipe("stdout"))?;
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            super::stream_session::read_codex_stdout_lines(stdout, sender)
        });
        let mut session = Self {
            child,
            stdin,
            lines: receiver,
            reader: Some(reader),
            codex_home,
            stderr_path,
            command_display: spawn.display_command(),
            model: selected_codex_model(config, request).map(ToOwned::to_owned),
            mcp_server_urls: codex_mcp_server_urls(request),
            next_id: 1,
            provider_thread_id: None,
        };
        session.initialize(invocation)?;
        Ok(session)
    }

    pub(super) fn is_compatible(&self, config: &LlmProviderConfig, request: &LlmRequest) -> bool {
        self.model == selected_codex_model(config, request).map(ToOwned::to_owned)
            && self.mcp_server_urls == codex_mcp_server_urls(request)
    }

    pub(super) fn run_turn(
        &mut self,
        request: &LlmRequest,
        invocation: &CodexInvocation<'_>,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<CodexTurnOutput> {
        self.ensure_thread(request, invocation, on_event)?;
        let id = self.next_request_id();
        self.send(json!({"id": id, "method": "turn/start", "params": {
            "threadId": self.provider_thread_id,
            "input": [{"type": "text", "text": prompt_text(request)}],
        }}))?;
        let content = self.read_turn(id, invocation, on_event)?;
        Ok(CodexTurnOutput {
            content,
            provider_session_id: self.provider_thread_id.clone(),
        })
    }

    fn ensure_thread(
        &mut self,
        request: &LlmRequest,
        invocation: &CodexInvocation<'_>,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        if self.provider_thread_id.is_some() {
            return Ok(());
        }
        let mut params =
            json!({"model": self.model, "sandbox": "read-only", "approvalPolicy": "never"});
        let response = if let Some(thread_id) = request.provider_session_id.as_deref() {
            params["threadId"] = json!(thread_id);
            let resumed = self.request("thread/resume", params.clone(), invocation)?;
            if missing_thread(&resumed) {
                params
                    .as_object_mut()
                    .expect("thread parameters")
                    .remove("threadId");
                self.request("thread/start", params, invocation)?
            } else {
                resumed
            }
        } else {
            self.request("thread/start", params, invocation)?
        };
        let thread = response_result(&response)?
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid_response(&response, "thread.id string"))?;
        self.provider_thread_id = Some(thread.to_owned());
        on_event(LlmStreamEvent::Session {
            provider_session_id: thread.to_owned(),
        })
    }

    fn read_turn(
        &mut self,
        request_id: u64,
        invocation: &CodexInvocation<'_>,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<String> {
        let mut turn = AppServerTurn::default();
        let mut pending = VecDeque::new();
        loop {
            let value = if turn.id.is_some() && !pending.is_empty() {
                pending.pop_front().expect("pending turn notification")
            } else {
                self.read_message(invocation)?
            };
            // Codex may publish notifications before acknowledging turn/start.
            if turn.id.is_none() && value.get("method").is_some() {
                pending.push_back(value);
                continue;
            }
            if value["id"].as_u64() == Some(request_id) && value.get("method").is_none() {
                turn.id = Some(
                    response_result(&value)?["turn"]["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| invalid_response(&value, "turn.id string"))?
                        .to_owned(),
                );
            }
            if let Some(content) = turn.consume(
                &value,
                self.provider_thread_id.as_deref(),
                invocation,
                on_event,
            )? {
                return Ok(content);
            }
        }
    }

    fn initialize(&mut self, invocation: &CodexInvocation<'_>) -> Result<()> {
        let response = self.request(
            "initialize",
            json!({"clientInfo": {
                "name": "lumvise-assistant-session", "version": env!("CARGO_PKG_VERSION")
            }}),
            invocation,
        )?;
        response_result(&response)?;
        self.send(json!({"method": "initialized"}))
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        invocation: &CodexInvocation<'_>,
    ) -> Result<Value> {
        let id = self.next_request_id();
        self.send(json!({"id": id, "method": method, "params": params}))?;
        loop {
            let response = self.read_message(invocation)?;
            if response["id"].as_u64() == Some(id) && response.get("method").is_none() {
                return Ok(response);
            }
        }
    }

    fn read_message(&mut self, invocation: &CodexInvocation<'_>) -> Result<Value> {
        loop {
            invocation.check(&self.command_display)?;
            let Some(line) = self.next_line()? else {
                continue;
            };
            let value: Value = serde_json::from_str(&line).map_err(|source| NeuralError::Json {
                value: line,
                expected: "Codex app-server JSON-RPC message".into(),
                source,
            })?;
            if value.get("method").is_some() && value.get("id").is_some() {
                // This client has no interactive approver. Never silently grant an
                // unexpected server request or leave it hanging until the deadline.
                self.send(json!({"id": value["id"], "error": {
                    "code": -32601, "message": "Lumvise does not support interactive app-server requests"
                }}))?;
                continue;
            }
            return Ok(value);
        }
    }

    fn send(&mut self, value: serde_json::Value) -> Result<()> {
        let bytes = serde_json::to_vec(&value).map_err(|source| NeuralError::Json {
            value: self.command_display.clone(),
            expected: "serializable Codex app-server request".to_string(),
            source,
        })?;
        self.stdin
            .write_all(&bytes)
            .map_err(|source| NeuralError::Io {
                value: self.command_display.clone(),
                expected: "writable Codex app-server stdin".to_string(),
                source,
            })?;
        self.stdin
            .write_all(b"\n")
            .map_err(|source| NeuralError::Io {
                value: self.command_display.clone(),
                expected: "writable Codex app-server newline".to_string(),
                source,
            })?;
        self.stdin.flush().map_err(|source| NeuralError::Io {
            value: self.command_display.clone(),
            expected: "flushable Codex app-server stdin".to_string(),
            source,
        })
    }

    fn next_line(&mut self) -> Result<Option<String>> {
        match self.lines.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line.map(Some).map_err(|source| NeuralError::Io {
                value: self.command_display.clone(),
                expected: "Codex app-server stdout line".to_string(),
                source,
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.closed_before_response_error()),
        }
    }

    fn closed_before_response_error(&self) -> NeuralError {
        NeuralError::ProcessFailed {
            command: self.command_display.clone(),
            status: "closed before response".to_string(),
            diagnostics: ProcessFailureDiagnostics::from_stderr(
                std::fs::read_to_string(&self.stderr_path).unwrap_or_default(),
            ),
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

impl Drop for CodexAppServerSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.reader.take();
        let _ = std::fs::remove_file(&self.stderr_path);
        if let Some(codex_home) = self.codex_home.take() {
            let _ = std::fs::remove_dir_all(codex_home);
        }
    }
}

#[derive(Default)]
struct AppServerTurn {
    id: Option<String>,
    content: String,
    state: CodexStreamState,
}

impl AppServerTurn {
    fn consume(
        &mut self,
        value: &Value,
        thread_id: Option<&str>,
        invocation: &CodexInvocation<'_>,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<Option<String>> {
        let params = &value["params"];
        let event_turn = params["turnId"]
            .as_str()
            .or_else(|| params["turn"]["id"].as_str());
        if params["threadId"].as_str() != thread_id
            || event_turn != self.id.as_deref()
            || self.id.is_none()
        {
            return Ok(None);
        }
        match value["method"].as_str() {
            Some("item/agentMessage/delta") => {
                if let Some(delta) = params["delta"].as_str() {
                    self.state.emit_content(
                        delta.to_owned(),
                        invocation.stream.clone(),
                        on_event,
                    )?;
                }
            }
            Some("item/completed") => self.capture_message(&params["item"]),
            Some("turn/completed") => {
                return self.finish(&params["turn"], invocation, on_event).map(Some);
            }
            _ => {}
        }
        Ok(None)
    }

    fn capture_message(&mut self, item: &Value) {
        if item["type"] == "agentMessage" {
            if let Some(text) = item["text"].as_str() {
                self.content = text.to_owned();
            }
        }
    }

    fn finish(
        &mut self,
        turn: &Value,
        invocation: &CodexInvocation<'_>,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<String> {
        if turn["status"] != "completed" {
            return Err(invalid_response(turn, "completed Codex turn"));
        }
        if let Some(items) = turn["items"].as_array() {
            for item in items {
                self.capture_message(item);
            }
        }
        if self.state.emitted_content() {
            self.state.emit_complete(on_event)?;
        } else {
            emit_stream_events(self.content.clone(), invocation.stream.clone(), on_event)?;
        }
        Ok(self.content.clone())
    }
}

fn response_result(value: &Value) -> Result<&Value> {
    if value.get("error").is_some() {
        return Err(invalid_response(
            value,
            "successful Codex app-server response",
        ));
    }
    value
        .get("result")
        .ok_or_else(|| invalid_response(value, "Codex app-server result"))
}

fn invalid_response(value: &Value, expected: &str) -> NeuralError {
    NeuralError::InvalidValue {
        value: value.to_string(),
        expected: expected.to_owned(),
    }
}

fn missing_thread(value: &Value) -> bool {
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    message.contains("thread not found") || message.contains("no rollout found")
}
