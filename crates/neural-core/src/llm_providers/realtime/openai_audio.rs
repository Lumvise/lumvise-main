//! OpenAI GA Realtime audio wire dialect; no application session state lives here.
use super::audio::{
    AudioSessionCommand, AudioSessionEvent, decode_pcm, invalid_audio, required_string,
};
use super::audio_wire::{AudioToolCall, AudioWireDialect, AudioWireEvent};
use super::events::{RealtimeSessionEvent, normalize_realtime_session_event};
use crate::error::Result;
use base64::Engine;
use serde_json::{Value, json};

pub(super) struct OpenAiAudioWire {
    pub model: String,
    pub voice: String,
    pub instructions: String,
    pub tools: Vec<Value>,
    pub response_id: String,
    pub response_active: bool,
}

impl AudioWireDialect for OpenAiAudioWire {
    fn setup(&self) -> Value {
        json!({"type":"session.update", "session": {
            "type":"realtime", "model":self.model, "output_modalities":["audio"],
            "instructions":self.instructions, "tools":self.tools, "tool_choice":"auto",
            "audio": {"input": {"format":{"type":"audio/pcm","rate":24000},
                "transcription":{"model":"gpt-4o-mini-transcribe"},
                "turn_detection":{"type":"server_vad","create_response":true,"interrupt_response":true}},
                "output":{"format":{"type":"audio/pcm","rate":24000},"voice":self.voice}}
        }})
    }

    fn command(&mut self, command: AudioSessionCommand) -> Result<Vec<Value>> {
        match command {
            AudioSessionCommand::Pcm(pcm) => Ok(vec![json!({"type":"input_audio_buffer.append",
                "audio":base64::engine::general_purpose::STANDARD.encode(pcm)})]),
            AudioSessionCommand::Text(text) => Ok(vec![
                json!({"type":"conversation.item.create",
                "item":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}),
                json!({"type":"response.create"}),
            ]),
            AudioSessionCommand::Interrupt { item_id, played_ms } => {
                Ok(self.interrupt(item_id, played_ms))
            }
            AudioSessionCommand::Pause => {
                let mut events = self.interrupt(None, 0);
                events.push(json!({"type":"input_audio_buffer.clear"}));
                Ok(events)
            }
            AudioSessionCommand::PlaybackStopped { item_id, played_ms } => Ok(vec![
                json!({"type":"conversation.item.truncate","item_id":item_id,"content_index":0,"audio_end_ms":played_ms}),
            ]),
            AudioSessionCommand::UserTurnEnded
            | AudioSessionCommand::Resume
            | AudioSessionCommand::Close => Ok(vec![]),
        }
    }

    fn receive(&mut self, message: Value) -> Result<Vec<AudioWireEvent>> {
        match message["type"].as_str().unwrap_or_default() {
            "response.created" => self.started(&message),
            "input_audio_buffer.speech_started" => {
                self.response_active = false;
                Ok(vec![
                    session(AudioSessionEvent::Interrupted),
                    session(AudioSessionEvent::InputStarted),
                ])
            }
            "input_audio_buffer.speech_stopped" => {
                Ok(vec![session(AudioSessionEvent::InputStopped)])
            }
            "response.done" => self.completed(message),
            "conversation.item.truncated" => Ok(vec![]),
            _ => self.normalized(message),
        }
    }

    fn tool_result(&self, id: &str, _name: &str, result: Value, last: bool) -> Vec<Value> {
        let mut messages = vec![
            json!({"type":"conversation.item.create", "item":{"type":"function_call_output",
            "call_id":id,"output":result.to_string()}}),
        ];
        if last {
            messages.push(json!({"type":"response.create"}));
        }
        messages
    }
}

impl OpenAiAudioWire {
    fn completed(&mut self, message: Value) -> Result<Vec<AudioWireEvent>> {
        let mut events = self.normalized(message.clone())?;
        if message["response"]["status"] != "completed" {
            return Ok(events);
        }
        for item in message["response"]["output"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if item["type"] == "function_call" {
                events.push(AudioWireEvent::Tool(tool_call(item)?));
            }
        }
        if events
            .iter()
            .any(|event| matches!(event, AudioWireEvent::Tool(_)))
        {
            events.retain(|event| {
                !matches!(
                    event,
                    AudioWireEvent::Session(AudioSessionEvent::ResponseFinished { .. })
                )
            });
        }
        Ok(events)
    }

    fn interrupt(&mut self, item_id: Option<String>, played_ms: u64) -> Vec<Value> {
        let mut events = Vec::new();
        if self.response_active {
            events.push(json!({"type":"response.cancel"}));
        }
        self.response_active = false;
        if let Some(item_id) = item_id {
            events.push(json!({"type":"conversation.item.truncate",
            "item_id":item_id,"content_index":0,"audio_end_ms":played_ms}));
        }
        events
    }

    fn started(&mut self, message: &Value) -> Result<Vec<AudioWireEvent>> {
        self.response_id = required_string(&message["response"], "id")?;
        self.response_active = true;
        Ok(vec![session(AudioSessionEvent::ResponseStarted {
            response_id: self.response_id.clone(),
        })])
    }

    fn normalized(&mut self, message: Value) -> Result<Vec<AudioWireEvent>> {
        let Some(event) = normalize_realtime_session_event("openai_realtime", &message)? else {
            return Ok(vec![]);
        };
        let response_id = message["response_id"]
            .as_str()
            .or_else(|| message["response"]["id"].as_str())
            .unwrap_or(&self.response_id)
            .to_string();
        if !response_id.is_empty() && response_id != self.response_id {
            return Ok(vec![]);
        }
        let output = match event {
            RealtimeSessionEvent::SessionUpdated {
                provider_session_id,
            } => Some(AudioSessionEvent::Ready {
                provider_session_id,
            }),
            RealtimeSessionEvent::TranscriptDelta { item_id, text } => {
                Some(AudioSessionEvent::Transcript {
                    item_id,
                    text,
                    final_revision: false,
                })
            }
            RealtimeSessionEvent::TranscriptFinal { item_id, text } => {
                Some(AudioSessionEvent::Transcript {
                    item_id,
                    text,
                    final_revision: true,
                })
            }
            RealtimeSessionEvent::ResponseTextDelta { text, .. } => {
                Some(AudioSessionEvent::ResponseText {
                    response_id,
                    text,
                    final_revision: false,
                })
            }
            RealtimeSessionEvent::ResponseTextFinal { text, .. } => {
                Some(AudioSessionEvent::ResponseText {
                    response_id,
                    text,
                    final_revision: true,
                })
            }
            RealtimeSessionEvent::ResponseAudioDelta { item_id, .. } => {
                if !self.response_active {
                    return Ok(vec![]);
                }
                Some(AudioSessionEvent::Audio {
                    response_id,
                    item_id,
                    pcm: decode_pcm(&message, "delta")?,
                    sample_rate: 24000,
                })
            }
            RealtimeSessionEvent::Complete => {
                self.response_active = false;
                Some(AudioSessionEvent::ResponseFinished { response_id })
            }
            RealtimeSessionEvent::Cancelled { .. } | RealtimeSessionEvent::Interrupted { .. } => {
                self.response_active = false;
                Some(AudioSessionEvent::Interrupted)
            }
            RealtimeSessionEvent::Error { message } => {
                return Err(invalid_audio(
                    message,
                    "successful Realtime conversation event",
                ));
            }
            _ => None,
        };
        Ok(output.into_iter().map(session).collect())
    }
}

fn session(event: AudioSessionEvent) -> AudioWireEvent {
    AudioWireEvent::Session(event)
}

fn tool_call(message: &Value) -> Result<AudioToolCall> {
    let raw = required_string(message, "arguments")?;
    let arguments =
        serde_json::from_str(&raw).map_err(|_| invalid_audio(raw, "JSON tool arguments"))?;
    Ok(AudioToolCall {
        id: required_string(message, "call_id")?,
        name: required_string(message, "name")?,
        arguments,
    })
}
