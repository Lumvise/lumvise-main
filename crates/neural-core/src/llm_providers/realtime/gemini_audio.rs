//! Gemini Live native audio dialect. Each completed generation gets a distinct local response id.
use super::audio::{
    AudioSessionCommand, AudioSessionEvent, decode_pcm, invalid_audio, required_string,
};
use super::audio_wire::{AudioToolCall, AudioWireDialect, AudioWireEvent};
use crate::error::Result;
use base64::Engine;
use serde_json::{Value, json};

pub(super) struct GeminiAudioWire {
    pub model: String,
    pub voice: String,
    pub instructions: String,
    pub tools: Vec<Value>,
    pub turn: u64,
    pub response_active: bool,
    pub input_transcript: String,
    pub suppress_response: bool,
    pub resumption_handle: Option<String>,
    pub reconnect_requested: bool,
}

impl AudioWireDialect for GeminiAudioWire {
    fn setup(&self) -> Value {
        let mut setup = json!({"setup":{"model":format!("models/{}", self.model.trim_start_matches("models/")),
            "generationConfig":{"responseModalities":["AUDIO"],
                "speechConfig":{"voiceConfig":{"prebuiltVoiceConfig":{"voiceName":self.voice}}}},
            "systemInstruction":{"parts":[{"text":self.instructions}]},
            "inputAudioTranscription":{},"outputAudioTranscription":{},
            "sessionResumption":{},"contextWindowCompression":{"slidingWindow":{}},
            "tools":[{"functionDeclarations":self.tools}]}});
        if let Some(handle) = &self.resumption_handle {
            setup["setup"]["sessionResumption"]["handle"] = json!(handle);
        }
        setup
    }

    fn command(&mut self, command: AudioSessionCommand) -> Result<Vec<Value>> {
        match command {
            AudioSessionCommand::Pcm(pcm) => Ok(vec![json!({"realtimeInput":{"audio":{
                "mimeType":"audio/pcm;rate=24000","data":base64::engine::general_purpose::STANDARD.encode(pcm)}}})]),
            AudioSessionCommand::Text(text) => Ok(vec![json!({"clientContent":{"turns":[{
                "role":"user","parts":[{"text":text}]}],"turnComplete":true}})]),
            AudioSessionCommand::Pause => {
                self.suppress_response = self.response_active;
                Ok(vec![
                    json!({"realtimeInput":{"audioStreamEnd":true}}),
                    json!({"clientContent":{"turnComplete":false}}),
                ])
            }
            AudioSessionCommand::Interrupt { .. } => {
                self.suppress_response = self.response_active;
                Ok(vec![json!({"clientContent":{"turnComplete":false}})])
            }
            AudioSessionCommand::PlaybackStopped { .. }
            | AudioSessionCommand::UserTurnEnded
            | AudioSessionCommand::Resume
            | AudioSessionCommand::Close => Ok(vec![]),
        }
    }

    fn receive(&mut self, message: Value) -> Result<Vec<AudioWireEvent>> {
        if message.get("error").is_some() {
            return Err(invalid_audio(
                &message["error"],
                "successful Gemini Live event",
            ));
        }
        if let Some(update) = message.get("sessionResumptionUpdate") {
            self.resumption_handle = if update["resumable"] == true {
                update["newHandle"]
                    .as_str()
                    .filter(|handle| !handle.is_empty())
                    .map(str::to_owned)
            } else {
                None
            };
            return Ok(self.reconnect_when_ready());
        }
        if message.get("goAway").is_some() {
            self.reconnect_requested = true;
            return Ok(self.reconnect_when_ready());
        }
        if message.get("setupComplete").is_some() {
            return Ok(vec![session(AudioSessionEvent::Ready {
                provider_session_id: None,
            })]);
        }
        if let Some(calls) = message
            .pointer("/toolCall/functionCalls")
            .and_then(Value::as_array)
        {
            return calls.iter().map(tool_call).collect();
        }
        if let Some(ids) = message
            .pointer("/toolCallCancellation/ids")
            .and_then(Value::as_array)
        {
            return Ok(vec![AudioWireEvent::ToolsCancelled(
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )]);
        }
        self.content(&message["serverContent"])
    }

    fn tool_result(&self, id: &str, name: &str, result: Value, _last: bool) -> Vec<Value> {
        vec![
            json!({"toolResponse":{"functionResponses":[{"id":id,"name":name,"response":{"result":result}}]}}),
        ]
    }
}

impl GeminiAudioWire {
    fn reconnect_when_ready(&mut self) -> Vec<AudioWireEvent> {
        if !self.reconnect_requested || self.resumption_handle.is_none() {
            return vec![];
        }
        self.reconnect_requested = false;
        vec![AudioWireEvent::Reconnect]
    }

    fn content(&mut self, content: &Value) -> Result<Vec<AudioWireEvent>> {
        let mut events = Vec::new();
        if content["interrupted"] == true {
            self.response_active = false;
            events.push(session(AudioSessionEvent::Interrupted));
        }
        if let Some(text) = content
            .pointer("/inputTranscription/text")
            .and_then(Value::as_str)
        {
            self.input_transcript.push_str(text);
            events.push(session(AudioSessionEvent::Transcript {
                item_id: None,
                text: text.into(),
                final_revision: false,
            }));
        }
        if content["turnComplete"] == true && !self.input_transcript.is_empty() {
            events.push(session(AudioSessionEvent::Transcript {
                item_id: None,
                text: std::mem::take(&mut self.input_transcript),
                final_revision: true,
            }));
        }
        if self.suppress_response {
            if content["turnComplete"] == true {
                self.suppress_response = false;
                self.response_active = false;
            }
            return Ok(events);
        }
        self.response(content, &mut events)?;
        Ok(events)
    }

    fn response(&mut self, content: &Value, events: &mut Vec<AudioWireEvent>) -> Result<()> {
        if content.get("modelTurn").is_some() || content.get("outputTranscription").is_some() {
            self.start_response(events);
        }
        if let Some(text) = content
            .pointer("/outputTranscription/text")
            .and_then(Value::as_str)
        {
            events.push(session(AudioSessionEvent::ResponseText {
                response_id: self.response_id(),
                text: text.into(),
                final_revision: false,
            }));
        }
        for part in content
            .pointer("/modelTurn/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(audio) = part.get("inlineData") {
                self.audio(audio, events)?;
            }
        }
        if content["turnComplete"] == true && self.response_active {
            self.response_active = false;
            events.push(session(AudioSessionEvent::ResponseFinished {
                response_id: self.response_id(),
            }));
        }
        Ok(())
    }

    fn start_response(&mut self, events: &mut Vec<AudioWireEvent>) {
        if self.response_active {
            return;
        }
        self.turn += 1;
        self.response_active = true;
        events.push(session(AudioSessionEvent::ResponseStarted {
            response_id: self.response_id(),
        }));
    }

    fn response_id(&self) -> String {
        format!("gemini-audio-{}", self.turn)
    }

    fn audio(&self, audio: &Value, events: &mut Vec<AudioWireEvent>) -> Result<()> {
        let mime = required_string(audio, "mimeType")?;
        if !mime.starts_with("audio/pcm") {
            return Err(invalid_audio(mime, "audio/pcm response"));
        }
        let rate = mime
            .split(';')
            .find_map(|part| part.trim().strip_prefix("rate="))
            .unwrap_or("24000")
            .parse::<u32>()
            .map_err(|_| invalid_audio(&mime, "PCM sample rate"))?;
        if rate == 0 {
            return Err(invalid_audio(&mime, "positive PCM sample rate"));
        }
        events.push(session(AudioSessionEvent::Audio {
            response_id: self.response_id(),
            item_id: None,
            pcm: decode_pcm(audio, "data")?,
            sample_rate: rate,
        }));
        Ok(())
    }
}

fn session(event: AudioSessionEvent) -> AudioWireEvent {
    AudioWireEvent::Session(event)
}

fn tool_call(call: &Value) -> Result<AudioWireEvent> {
    let arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(invalid_audio(arguments, "object tool arguments"));
    }
    Ok(AudioWireEvent::Tool(AudioToolCall {
        id: required_string(call, "id")?,
        name: required_string(call, "name")?,
        arguments,
    }))
}
