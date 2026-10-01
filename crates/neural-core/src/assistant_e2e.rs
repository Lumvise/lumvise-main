//! Deterministic in-process neural adapters used only by the assistant end-to-end fixture.
//!
//! These adapters deliberately do not spawn a desktop process or call a centralized resource.
//! They make the resource-server fixture self-contained while exercising the same Neural Core
//! LLM and speech contracts as production adapters.

use crate::{
    LlmProviderRegistry, NeuralError, Result, SpeechRecognizer, SpeechSynthesizer,
    llm_providers::{
        LlmCapabilitySupport, LlmProviderCapabilities, LlmRequest, LlmResponse, LlmStreamEvent,
        contract::{LlmProvider, LlmStreamEventSink},
    },
    process::StreamControl,
    text2voice::{
        Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
    },
    types::EngineMetadata,
    voice2text::{
        Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent, Voice2TextStreamEventSink,
    },
};
use lumvise_resource_routing::InvocationControl;

pub const PROVIDER_ID: &str = "assistant-e2e";
pub const MODEL_ID: &str = "assistant-e2e-fixture";
pub const SPEECH_ADAPTER_ID: &str = "assistant-e2e-speech";

/// Builds the deterministic LLM registry used by the assistant end-to-end fixture.
pub fn llm_registry() -> Result<LlmProviderRegistry> {
    LlmProviderRegistry::from_provider_instances(vec![Box::new(DeterministicLlmProvider)])
}

/// Returns the deterministic speech pair used by the assistant end-to-end fixture.
pub fn speech_adapters() -> (
    DeterministicSpeechRecognizer,
    DeterministicSpeechSynthesizer,
) {
    (
        DeterministicSpeechRecognizer,
        DeterministicSpeechSynthesizer,
    )
}

pub struct DeterministicLlmProvider;

impl LlmProvider for DeterministicLlmProvider {
    fn provider_id(&self) -> &str {
        PROVIDER_ID
    }

    fn capabilities(&self) -> LlmProviderCapabilities {
        LlmProviderCapabilities {
            provider_id: PROVIDER_ID.into(),
            final_text_output: LlmCapabilitySupport::Supported,
            streamed_text_output: LlmCapabilitySupport::Supported,
            image_snapshot_input: LlmCapabilitySupport::Unsupported,
            live_audio_input: LlmCapabilitySupport::Unsupported,
            screen_frame_broadcast_input: LlmCapabilitySupport::Unsupported,
            native_audio_output: LlmCapabilitySupport::Unsupported,
        }
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        if request.messages.iter().any(|message| {
            message.role == "user"
                && message
                    .content
                    .contains("[assistant-e2e:provider-rejected]")
        }) {
            return Err(NeuralError::ProviderFailed {
                provider_id: PROVIDER_ID.into(),
                message: "deterministic provider rejection for transition replay".into(),
            });
        }
        Ok(LlmResponse {
            provider_id: PROVIDER_ID.into(),
            model: MODEL_ID.into(),
            content: "deterministic assistant-e2e response".into(),
            metadata: serde_json::json!({"fixture": true}),
        })
    }

    fn stream_with_events(
        &self,
        request: &LlmRequest,
        _: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        let response = self.complete(request)?;
        on_event(LlmStreamEvent::Session {
            provider_session_id: "assistant-e2e-session".into(),
        })?;
        on_event(LlmStreamEvent::ContentDelta {
            text: response.content.clone(),
        })?;
        on_event(LlmStreamEvent::FinalText {
            text: response.content,
        })?;
        on_event(LlmStreamEvent::Complete)
    }
}

pub struct DeterministicSpeechRecognizer;

impl SpeechRecognizer for DeterministicSpeechRecognizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }

    fn transcribe(
        &self,
        _: &Voice2TextRequest,
        control: &InvocationControl,
    ) -> Result<Voice2TextResponse> {
        ensure_active(control, "speech recognition")?;
        Ok(Voice2TextResponse {
            transcript: "deterministic transcript".into(),
            language: Some("en".into()),
            confidence: Some(1.0),
            segments: Vec::new(),
            metadata: metadata(),
        })
    }

    fn stream_with_events(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
        on_event: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()> {
        let response = self.transcribe(request, control)?;
        on_event(Voice2TextStreamEvent::TranscriptChunk {
            sequence: 0,
            text: response.transcript,
            is_final: true,
        })?;
        on_event(Voice2TextStreamEvent::Complete)
    }
}

pub struct DeterministicSpeechSynthesizer;

impl SpeechSynthesizer for DeterministicSpeechSynthesizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }

    fn synthesize(
        &self,
        _: &Text2VoiceRequest,
        control: &InvocationControl,
    ) -> Result<Text2VoiceResponse> {
        ensure_active(control, "speech synthesis")?;
        Ok(Text2VoiceResponse {
            audio: b"RIFF".to_vec(),
            media_type: "audio/wav".into(),
            sample_rate_hz: Some(24_000),
            metadata: metadata(),
        })
    }

    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        let response = self.synthesize(request, control)?;
        on_event(Text2VoiceStreamEvent::AudioChunk {
            sequence: 0,
            audio: response.audio,
            media_type: response.media_type,
        })?;
        on_event(Text2VoiceStreamEvent::Complete)
    }
}

fn metadata() -> EngineMetadata {
    EngineMetadata {
        engine_id: SPEECH_ADAPTER_ID.into(),
        model: Some(MODEL_ID.into()),
        metadata: serde_json::json!({"fixture": true}),
    }
}

fn ensure_active(control: &InvocationControl, operation: &str) -> Result<()> {
    if control.is_cancelled() || control.is_expired() {
        return Err(NeuralError::ProviderFailed {
            provider_id: SPEECH_ADAPTER_ID.into(),
            message: format!("{operation} cancelled or deadline elapsed before dispatch"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_models_a_rejected_turn_without_changing_normal_responses() {
        for (prompt, rejected) in [
            ("Hello", false),
            ("[assistant-e2e:provider-rejected]", true),
        ] {
            let request: LlmRequest = serde_json::from_value(serde_json::json!({
                "messages":[{"role":"user","content":prompt}], "stream":false
            }))
            .unwrap();
            let response = DeterministicLlmProvider.complete(&request);
            assert_eq!(
                matches!(response, Err(NeuralError::ProviderFailed { .. })),
                rejected
            );
            if !rejected {
                assert_eq!(
                    response.unwrap().content,
                    "deterministic assistant-e2e response"
                );
            }
        }
    }
}
