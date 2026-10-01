use crate::error::Result;
use crate::llm_providers::{LlmProviderCapabilities, LlmRequest, LlmResponse, LlmStreamEvent};
use crate::process::StreamControl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub type LlmStreamEventSink<'a> = dyn FnMut(LlmStreamEvent) -> Result<()> + 'a;
pub type LlmTextChunkSink<'a> = dyn FnMut(String) -> Result<()> + 'a;

/// Transport-neutral deadline and cancellation state for one provider call.
pub trait ProviderCallControl: Send + Sync {
    fn remaining(&self) -> Duration;
    fn is_cancelled(&self) -> bool;
    /// True only when the caller explicitly cancelled the operation.
    ///
    /// Deadline expiry is a recoverable turn failure, not a user cancellation.
    fn is_expired(&self) -> bool {
        self.remaining().is_zero()
    }
    /// Returns the original routing control when this call is backed by one.
    /// Providers that cross the central boundary must reuse it rather than
    /// allocating a fresh deadline/cancellation scope.
    fn invocation_control(&self) -> Option<&lumvise_resource_routing::InvocationControl> {
        None
    }
}

pub trait LlmProvider: Send + Sync {
    fn provider_id(&self) -> &str;

    fn capabilities(&self) -> LlmProviderCapabilities;

    /// Runs one duplex conversation until closed. Example: pass a capture command receiver
    /// and route `AudioSessionEvent::Audio` to the application's playback stream.
    fn run_audio_session(
        &self,
        _request: super::AudioSessionRequest,
        _input: super::AudioSessionInput,
        _on_event: &mut super::AudioSessionEventSink<'_>,
    ) -> Result<()> {
        Err(crate::NeuralError::ProviderFailed {
            provider_id: self.provider_id().into(),
            message: "direct audio unavailable; expected a provider with a duplex audio transport"
                .into(),
        })
    }

    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse>;

    fn complete_controlled(
        &self,
        request: &LlmRequest,
        control: &dyn ProviderCallControl,
    ) -> Result<LlmResponse> {
        if control.is_cancelled() {
            return Err(crate::NeuralError::ProcessCancelled {
                command: self.provider_id().into(),
            });
        }
        if control.is_expired() {
            return Err(crate::NeuralError::ProcessTimeout {
                command: self.provider_id().into(),
                timeout_ms: 0,
            });
        }
        self.complete(request)
    }

    fn stream(&self, request: &LlmRequest, control: StreamControl) -> Result<Vec<LlmStreamEvent>> {
        let mut events = Vec::new();
        self.stream_with_events(request, control, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        on_event: &mut LlmStreamEventSink<'_>,
    ) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmHttpRequest {
    pub timeout: Option<Duration>,
    pub endpoint: String,
    pub credential: String,
    pub headers: BTreeMap<String, String>,
    pub payload: Value,
}

pub trait LlmHttpClient: Send + Sync {
    fn post_json(&self, request: &LlmHttpRequest) -> Result<Value>;
    /// Retrieves a JSON inventory document. Implementations that only support
    /// completions fail closed; discovery never falls back to a static catalog.
    fn get_json(&self, request: &LlmHttpRequest) -> Result<Value> {
        Err(crate::NeuralError::ProviderFailed {
            provider_id: request.endpoint.clone(),
            message: "HTTP model discovery is not supported by this transport".to_string(),
        })
    }

    fn stream_text(&self, request: &LlmHttpRequest) -> Result<Vec<String>>;

    /// Reads chunks until the consumer completes or cancels its response.
    /// Override for incremental I/O; `Break(())` closes the response without reading its tail.
    /// Example: `client.stream_text_until(request, &mut |_| Ok(ControlFlow::Break(())))`.
    fn stream_text_until(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut dyn FnMut(String) -> Result<std::ops::ControlFlow<()>>,
    ) -> Result<()> {
        for chunk in self.stream_text(request)? {
            if on_chunk(chunk)?.is_break() {
                break;
            }
        }
        Ok(())
    }

    fn stream_text_with_chunks(
        &self,
        request: &LlmHttpRequest,
        on_chunk: &mut LlmTextChunkSink<'_>,
    ) -> Result<()> {
        for chunk in self.stream_text(request)? {
            on_chunk(chunk)?;
        }
        Ok(())
    }
}
