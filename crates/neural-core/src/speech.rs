//! Speech inference seam owned by Neural Core.
//!
//! Both local and centralized adapters implement these two interfaces. The
//! concrete native/spawned engines stay behind `Voice2TextService` and
//! `Text2VoiceService`, preserving PCM/WAV handling in their owning modules.

use lumvise_resource_routing::InvocationControl;

use crate::{
    Result,
    text2voice::{
        Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
    },
    voice2text::{Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEventSink},
};

pub trait SpeechRecognizer: Send + Sync {
    fn warmup(&self) -> Result<()>;
    fn transcribe(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
    ) -> Result<Voice2TextResponse>;
    fn stream_with_events(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
        on_event: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()>;
}

pub trait SpeechSynthesizer: Send + Sync {
    fn warmup(&self) -> Result<()>;
    fn synthesize(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
    ) -> Result<Text2VoiceResponse>;
    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()>;
}

pub(crate) fn ensure_active(control: &InvocationControl, operation: &str) -> Result<()> {
    if control.is_cancelled() || control.is_expired() {
        return Err(crate::NeuralError::ProviderFailed {
            provider_id: operation.into(),
            message: "speech invocation cancelled or deadline elapsed".into(),
        });
    }
    Ok(())
}

impl SpeechRecognizer for crate::voice2text::Voice2TextService {
    fn warmup(&self) -> Result<()> {
        Self::warmup(self)
    }

    fn transcribe(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
    ) -> Result<Voice2TextResponse> {
        ensure_active(control, "voice2text")?;
        let response = Self::transcribe(self, request)?;
        ensure_active(control, "voice2text")?;
        Ok(response)
    }

    fn stream_with_events(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
        on_event: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()> {
        ensure_active(control, "voice2text-stream")?;
        for event in Self::stream(self, request, crate::process::StreamControl::unbounded())? {
            ensure_active(control, "voice2text-stream")?;
            on_event(event)?;
        }
        Ok(())
    }
}

impl SpeechSynthesizer for crate::text2voice::Text2VoiceService {
    fn warmup(&self) -> Result<()> {
        Self::warmup(self)
    }

    fn synthesize(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
    ) -> Result<Text2VoiceResponse> {
        ensure_active(control, "text2voice")?;
        let response = Self::synthesize(self, request)?;
        ensure_active(control, "text2voice")?;
        Ok(response)
    }

    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        on_event: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        ensure_active(control, "text2voice-stream")?;
        let cancellation = control.cancellation_token();
        let mut guarded = |event: Text2VoiceStreamEvent| {
            ensure_active(control, "text2voice-stream")?;
            on_event(event)
        };
        Self::stream_with_events(
            self,
            request,
            crate::process::StreamControl::from_cancellation_token(cancellation),
            &mut guarded,
        )
    }
}
