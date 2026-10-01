//! Internal provider dialect shared by the duplex session driver.
use super::audio::{AudioSessionCommand, AudioSessionEvent};
use crate::error::Result;
use serde_json::Value;

pub(super) struct AudioToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

pub(super) enum AudioWireEvent {
    Session(AudioSessionEvent),
    Tool(AudioToolCall),
    ToolsCancelled(Vec<String>),
    Reconnect,
}

pub(super) trait AudioWireDialect: Send {
    fn setup(&self) -> Value;
    fn command(&mut self, command: AudioSessionCommand) -> Result<Vec<Value>>;
    fn receive(&mut self, message: Value) -> Result<Vec<AudioWireEvent>>;
    fn tool_result(&self, id: &str, name: &str, result: Value, last: bool) -> Vec<Value>;
}

pub(super) trait AudioSocket: Send {
    fn send(&mut self, message: Value) -> Result<()>;
    fn receive(&mut self) -> Result<Option<Value>>;
    fn close(&mut self);
    fn reconnect(&mut self) -> Result<()> {
        Err(super::audio::invalid_audio(
            "socket",
            "reconnect-capable audio transport",
        ))
    }
}
