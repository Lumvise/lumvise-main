//! One duplex socket loop. Tool I/O runs off-loop so microphone frames keep flowing.
use super::audio::{
    AudioSessionCommand, AudioSessionEvent, AudioSessionEventSink, AudioSessionInput, invalid_audio,
};
use super::audio_wire::{AudioSocket, AudioToolCall, AudioWireDialect, AudioWireEvent};
use crate::error::Result;
use crate::llm_providers::tool_invocation::{McpToolCatalog, tool_outcome_value};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::time::{Duration, Instant};

struct CompletedAudioTool {
    id: String,
    name: String,
    result: Result<Value>,
}

pub(super) struct AudioSessionDriver {
    socket: Box<dyn AudioSocket>,
    wire: Box<dyn AudioWireDialect>,
    tools: Arc<McpToolCatalog>,
    tool_sender: Sender<CompletedAudioTool>,
    tool_receiver: Receiver<CompletedAudioTool>,
    pending_tools: HashSet<String>,
    seen_tools: HashSet<String>,
    pending_input: VecDeque<AudioSessionCommand>,
    deadline: Option<Instant>,
    ready: bool,
    paused: bool,
}

impl AudioSessionDriver {
    pub(super) fn new(
        socket: Box<dyn AudioSocket>,
        wire: Box<dyn AudioWireDialect>,
        tools: Arc<McpToolCatalog>,
    ) -> Self {
        let (tool_sender, tool_receiver) = mpsc::channel();
        Self {
            socket,
            wire,
            tools,
            tool_sender,
            tool_receiver,
            pending_tools: HashSet::new(),
            seen_tools: HashSet::new(),
            pending_input: VecDeque::new(),
            deadline: Some(Instant::now() + Duration::from_secs(60)),
            ready: false,
            paused: false,
        }
    }

    pub(super) fn run(
        mut self,
        input: AudioSessionInput,
        on_event: &mut AudioSessionEventSink<'_>,
    ) -> Result<()> {
        let result = self.converse(input, on_event);
        self.socket.close();
        result?;
        on_event(AudioSessionEvent::Closed)
    }

    fn converse(
        &mut self,
        input: AudioSessionInput,
        on_event: &mut AudioSessionEventSink<'_>,
    ) -> Result<()> {
        self.socket.send(self.wire.setup())?;
        loop {
            if !self.accept_input(&input)? {
                return Ok(());
            }
            self.deliver_tools()?;
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(invalid_audio(
                    "audio response deadline exceeded",
                    "ready session or completed response within 60 seconds",
                ));
            }
            if let Some(message) = self.socket.receive()? {
                self.receive(message, on_event)?;
            }
        }
    }

    fn accept_input(&mut self, input: &AudioSessionInput) -> Result<bool> {
        loop {
            let command = match input.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(false),
            };
            if matches!(command, AudioSessionCommand::Close) {
                return Ok(false);
            }
            self.pending_input.push_back(command);
        }
        if self.ready {
            while let Some(command) = self.pending_input.pop_front() {
                self.send_command(command)?;
            }
        }
        Ok(true)
    }

    fn send_command(&mut self, command: AudioSessionCommand) -> Result<()> {
        match &command {
            AudioSessionCommand::Pcm(pcm) if pcm.is_empty() || pcm.len() % 2 != 0 => {
                return Err(invalid_audio(pcm.len(), "non-empty even PCM16 byte count"));
            }
            AudioSessionCommand::Pcm(_) if self.paused => return Ok(()),
            AudioSessionCommand::Pause => {
                self.paused = true;
                self.deadline = None;
            }
            AudioSessionCommand::Resume => self.paused = false,
            AudioSessionCommand::Text(_) | AudioSessionCommand::UserTurnEnded => {
                self.deadline = Some(Instant::now() + Duration::from_secs(60))
            }
            _ => {}
        }
        for message in self.wire.command(command)? {
            self.socket.send(message)?;
        }
        Ok(())
    }

    fn receive(&mut self, message: Value, on_event: &mut AudioSessionEventSink<'_>) -> Result<()> {
        for event in self.wire.receive(message)? {
            match event {
                AudioWireEvent::Session(event) => {
                    if self.paused && matches!(event, AudioSessionEvent::Audio { .. }) {
                        continue;
                    }
                    self.track_event(&event);
                    on_event(event)?;
                }
                AudioWireEvent::Tool(call) => self.start_tool(call),
                AudioWireEvent::Reconnect => {
                    self.ready = false;
                    self.deadline = Some(Instant::now() + Duration::from_secs(60));
                    self.socket.reconnect()?;
                    self.socket.send(self.wire.setup())?;
                }
                AudioWireEvent::ToolsCancelled(ids) => {
                    for id in ids {
                        self.pending_tools.remove(&id);
                    }
                }
            }
        }
        Ok(())
    }

    fn track_event(&mut self, event: &AudioSessionEvent) {
        if self.paused && !matches!(event, AudioSessionEvent::Ready { .. }) {
            return;
        }
        match event {
            AudioSessionEvent::Ready { .. } => {
                self.ready = true;
                self.deadline = None;
            }
            AudioSessionEvent::ResponseStarted { .. } | AudioSessionEvent::InputStopped => {
                self.deadline = Some(Instant::now() + Duration::from_secs(60))
            }
            AudioSessionEvent::ResponseFinished { .. } | AudioSessionEvent::Interrupted => {
                self.deadline = None
            }
            _ => {}
        }
    }

    fn start_tool(&mut self, call: AudioToolCall) {
        if !self.seen_tools.insert(call.id.clone()) {
            return;
        }
        self.pending_tools.insert(call.id.clone());
        self.deadline = Some(Instant::now() + Duration::from_secs(60));
        let tools = Arc::clone(&self.tools);
        let sender = self.tool_sender.clone();
        std::thread::spawn(move || {
            let result = if super::audio_connection::audio_tool(&call.name) {
                tool_outcome_value(tools.invoker().invoke(&call.name, call.arguments))
            } else {
                Ok(
                    serde_json::json!({"error":"Direct audio supplies speech and listens continuously; this speech tool is unavailable."}),
                )
            };
            let _ = sender.send(CompletedAudioTool {
                id: call.id,
                name: call.name,
                result,
            });
        });
    }

    fn deliver_tools(&mut self) -> Result<()> {
        while let Ok(tool) = self.tool_receiver.try_recv() {
            if !self.pending_tools.remove(&tool.id) {
                continue;
            }
            for message in self.wire.tool_result(
                &tool.id,
                &tool.name,
                tool.result?,
                self.pending_tools.is_empty() && !self.paused,
            ) {
                self.socket.send(message)?;
            }
            if !self.paused {
                self.deadline = Some(Instant::now() + Duration::from_secs(60));
            }
        }
        Ok(())
    }
}
