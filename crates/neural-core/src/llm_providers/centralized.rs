//! Centralized adapters that keep the LLM and speech owning interfaces free of
//! HTTP, authentication, and protocol details.

use std::{collections::BTreeMap, sync::Arc};

use lumvise_resource_routing::{
    InvocationControl, ResourceInvocationClient,
    protocol::{
        InvocationEnvelopeV1, InvocationTerminalStatusV1, LlmCompleteV1, LlmProviderDescriptorV1,
        LlmStreamV1, SpeechToTextStreamV1, SpeechToTextV1, TextToSpeechStreamV1, TextToSpeechV1,
        TypedBinaryChunkV1, invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    NeuralError, Result,
    process::StreamControl,
    speech::{SpeechRecognizer, SpeechSynthesizer},
    text2voice::{
        Text2VoiceRequest, Text2VoiceResponse, Text2VoiceStreamEvent, Text2VoiceStreamEventSink,
    },
    types::EngineMetadata,
    voice2text::{
        Voice2TextRequest, Voice2TextResponse, Voice2TextStreamEvent, Voice2TextStreamEventSink,
    },
};

use super::{
    LlmCapabilitySupport, LlmMessage, LlmProviderCapabilities, LlmRequest, LlmResponse,
    LlmStreamEvent,
};
use super::{
    contract::{LlmProvider, LlmStreamEventSink, ProviderCallControl},
    registry::LlmProviderRegistry,
};

/// An opaque desktop-bound MCP route.  The local URL never appears in an
/// `InvocationEnvelopeV1`; only this invocation-specific identifier does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CentralMcpRoute {
    pub route_id: String,
    pub local_url: String,
}

/// Local reverse-MCP boundary.  Implementations must reject route IDs not
/// bound to the current invocation before issuing a loopback request.
pub trait LlmMcpTransport: Send + Sync {
    fn bind(
        &self,
        request_id: &str,
        servers: &[super::LlmMcpServerConfig],
    ) -> Result<Vec<CentralMcpRoute>>;
    fn invoke_bound(
        &self,
        request_id: &str,
        route_id: &str,
        method: &str,
        parameters: &[TypedBinaryChunkV1],
        control: &InvocationControl,
    ) -> Result<Vec<TypedBinaryChunkV1>>;
}

/// Maps only original scoped MCP URLs to random opaque route IDs.  It is
/// deliberately a binding registry rather than a URL pass-through.
#[derive(Default)]
pub struct ScopedMcpTransport {
    bindings: parking_lot::Mutex<BTreeMap<String, BTreeMap<String, String>>>,
}

impl LlmMcpTransport for ScopedMcpTransport {
    fn bind(
        &self,
        request_id: &str,
        servers: &[super::LlmMcpServerConfig],
    ) -> Result<Vec<CentralMcpRoute>> {
        if request_id.trim().is_empty() {
            return Err(invalid("a non-empty invocation request ID"));
        }
        let mut bound = BTreeMap::new();
        let routes = servers
            .iter()
            .map(|server| {
                if !is_loopback_url(&server.url) {
                    return Err(invalid("a scoped loopback MCP URL"));
                }
                let route_id = Uuid::new_v4().to_string();
                bound.insert(route_id.clone(), server.url.clone());
                Ok(CentralMcpRoute {
                    route_id,
                    local_url: server.url.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.bindings.lock().insert(request_id.to_owned(), bound);
        Ok(routes)
    }

    fn invoke_bound(
        &self,
        request_id: &str,
        route_id: &str,
        _method: &str,
        _parameters: &[TypedBinaryChunkV1],
        control: &InvocationControl,
    ) -> Result<Vec<TypedBinaryChunkV1>> {
        if control.is_cancelled() || control.is_expired() {
            return Err(control_error("reverse MCP"));
        }
        let bindings = self.bindings.lock();
        let Some(routes) = bindings.get(request_id) else {
            return Err(invalid("a live parent invocation"));
        };
        if !routes.contains_key(route_id) {
            return Err(invalid("a route ID bound to this parent invocation"));
        }
        // The existing direct MCP executor remains the sole HTTP implementation.
        // This registry intentionally validates the scope before that executor is
        // called by composition code; it never turns a provider-supplied ID into
        // a URL itself.
        Err(NeuralError::ProviderFailed {
            provider_id: "centralized-mcp".into(),
            message: "reverse MCP execution must be supplied by the scoped desktop transport"
                .into(),
        })
    }
}

/// One remote provider descriptor becomes exactly one `LlmProvider` instance.
pub struct CentralizedLlmProvider {
    provider_id: String,
    capabilities: LlmProviderCapabilities,
    client: Arc<dyn ResourceInvocationClient>,
    client_instance_id: String,
    mcp: Arc<dyn LlmMcpTransport>,
}

impl CentralizedLlmProvider {
    pub fn new(
        descriptor: LlmProviderDescriptorV1,
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: impl Into<String>,
        mcp: Arc<dyn LlmMcpTransport>,
    ) -> Result<Self> {
        if descriptor.provider_id.trim().is_empty() {
            return Err(invalid("a ready provider ID"));
        }
        let mut capabilities: LlmProviderCapabilities =
            serde_json::from_slice(&descriptor.capabilities).unwrap_or_else(|_| {
                LlmProviderCapabilities {
                    provider_id: descriptor.provider_id.clone(),
                    final_text_output: LlmCapabilitySupport::Supported,
                    streamed_text_output: LlmCapabilitySupport::Supported,
                    image_snapshot_input: LlmCapabilitySupport::ModelDependent,
                    live_audio_input: LlmCapabilitySupport::ModelDependent,
                    screen_frame_broadcast_input: LlmCapabilitySupport::ModelDependent,
                    native_audio_output: LlmCapabilitySupport::Unsupported,
                }
            });
        capabilities.provider_id = descriptor.provider_id.clone();
        Ok(Self {
            provider_id: descriptor.provider_id,
            capabilities,
            client,
            client_instance_id: client_instance_id.into(),
            mcp,
        })
    }

    pub fn registry_from_readiness(
        descriptors: Vec<LlmProviderDescriptorV1>,
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: impl Into<String>,
        mcp: Arc<dyn LlmMcpTransport>,
    ) -> Result<LlmProviderRegistry> {
        let client_instance_id = client_instance_id.into();
        let mut concurrency = Vec::new();
        let mut instances: Vec<Box<dyn LlmProvider>> = Vec::new();
        for descriptor in descriptors {
            concurrency.push((descriptor.provider_id.clone(), descriptor.concurrency));
            instances.push(Box::new(Self::new(
                descriptor,
                Arc::clone(&client),
                client_instance_id.clone(),
                Arc::clone(&mcp),
            )?));
        }
        let mut registry = LlmProviderRegistry::from_provider_instances(instances)?;
        for (provider_id, limit) in concurrency {
            registry = registry.with_completion_concurrency(&provider_id, limit.max(1) as usize);
        }
        Ok(registry)
    }

    fn invoke(
        &self,
        request: &LlmRequest,
        control: &InvocationControl,
        stream: bool,
    ) -> Result<Vec<InvocationEnvelopeV1>> {
        if control.is_cancelled() || control.is_expired() {
            return Err(control_error(&self.provider_id));
        }
        let request_id = Uuid::new_v4().to_string();
        let routes = self.mcp.bind(&request_id, &request.mcp_servers)?;
        let operation = LlmCompleteV1 {
            provider_id: self.provider_id.clone(),
            model_override: request.model.clone(),
            conversation_id: request.conversation_id.clone(),
            provider_session_id: request.provider_session_id.clone(),
            input: encode_llm_input(&request.messages, &request.modality_inputs)?,
            scoped_mcp_route_ids: routes.into_iter().map(|route| route.route_id).collect(),
        };
        let operation = if stream {
            Operation::LlmStream(LlmStreamV1 {
                request: Some(operation),
            })
        } else {
            Operation::LlmComplete(operation)
        };
        let envelope = start_envelope(
            request_id,
            self.client_instance_id.clone(),
            operation,
            control,
        );
        self.client
            .invoke(&[envelope], control)
            .map_err(|error| transport_error(&self.provider_id, error))
    }

    fn complete_with_control(
        &self,
        request: &LlmRequest,
        control: &InvocationControl,
    ) -> Result<LlmResponse> {
        let envelopes = self.invoke(request, control, false)?;
        let terminal = terminal_for(&envelopes)?;
        ensure_completed(&self.provider_id, terminal)?;
        let Some(TerminalResult::Llm(result)) = terminal.result.as_ref() else {
            return Err(invalid("an LLM terminal result"));
        };
        let output = result
            .output
            .iter()
            .find(|chunk| chunk.type_name == "lumvise.llm.text.v1")
            .ok_or_else(|| invalid("a lumvise.llm.text.v1 output"))?;
        let content =
            String::from_utf8(output.bytes.clone()).map_err(|_| invalid("UTF-8 LLM output"))?;
        let metadata = output
            .metadata_json
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(|_| invalid("UTF-8 JSON LLM metadata"))?
            .unwrap_or(Value::Null);
        Ok(LlmResponse {
            provider_id: self.provider_id.clone(),
            model: request.model.clone().unwrap_or_default(),
            content,
            metadata,
        })
    }
}

impl LlmProvider for CentralizedLlmProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }
    fn capabilities(&self) -> LlmProviderCapabilities {
        self.capabilities.clone()
    }
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        self.complete_with_control(request, &InvocationControl::sixty_seconds())
    }
    fn complete_controlled(
        &self,
        request: &LlmRequest,
        control: &dyn ProviderCallControl,
    ) -> Result<LlmResponse> {
        if control.is_cancelled() || control.remaining().is_zero() {
            return Err(control_error(&self.provider_id));
        }
        let invocation =
            control
                .invocation_control()
                .ok_or_else(|| NeuralError::ProviderFailed {
                    provider_id: self.provider_id.clone(),
                    message: "centralized execution requires the caller's InvocationControl".into(),
                })?;
        self.complete_with_control(request, invocation)
    }
    fn stream_with_events(
        &self,
        request: &LlmRequest,
        control: StreamControl,
        sink: &mut LlmStreamEventSink<'_>,
    ) -> Result<()> {
        if control.is_cancelled() {
            return Err(control_error(&self.provider_id));
        }
        let invocation = InvocationControl::sixty_seconds();
        let envelopes = self.invoke(request, &invocation, true)?;
        let mut expected = 0_u64;
        for envelope in &envelopes {
            match &envelope.payload {
                Some(Payload::LlmStreamEvent(event)) => {
                    if event.event_sequence != expected {
                        return Err(invalid("ordered LLM stream event sequences"));
                    }
                    expected += 1;
                    match event.event.as_ref() {
                        Some(lumvise_resource_routing::protocol::llm_stream_event_v1::Event::TextDelta(text)) => sink(LlmStreamEvent::ContentDelta { text: text.clone() })?,
                        Some(lumvise_resource_routing::protocol::llm_stream_event_v1::Event::Binary(chunk)) => {
                            let text = String::from_utf8(chunk.bytes.clone()).map_err(|_| invalid("UTF-8 streamed LLM text"))?;
                            sink(LlmStreamEvent::ContentDelta { text })?
                        }
                        Some(lumvise_resource_routing::protocol::llm_stream_event_v1::Event::ToolCall(_)) => return Err(invalid("a server-handled reverse MCP result")),
                        None => return Err(invalid("a typed LLM stream event")),
                    }
                }
                Some(Payload::Terminal(terminal)) => {
                    ensure_completed(&self.provider_id, terminal)?;
                    sink(LlmStreamEvent::Complete)?;
                    return Ok(());
                }
                _ => return Err(invalid("LLM stream events followed by a terminal")),
            }
        }
        Err(invalid("a terminal LLM stream frame"))
    }
}

pub struct CentralizedSpeechRecognizer {
    client: Arc<dyn ResourceInvocationClient>,
    client_instance_id: String,
}
pub struct CentralizedSpeechSynthesizer {
    client: Arc<dyn ResourceInvocationClient>,
    client_instance_id: String,
}

impl CentralizedSpeechRecognizer {
    pub fn new(
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            client_instance_id: client_instance_id.into(),
        }
    }
    fn invoke(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
        stream: bool,
    ) -> Result<Vec<InvocationEnvelopeV1>> {
        let op = SpeechToTextV1 {
            audio: request.audio.clone(),
            media_type: request.media_type.clone(),
            model: request.model.clone(),
        };
        let operation = if stream {
            Operation::SpeechToTextStream(SpeechToTextStreamV1 { request: Some(op) })
        } else {
            Operation::SpeechToText(op)
        };
        self.client
            .invoke(
                &[start_envelope(
                    Uuid::new_v4().to_string(),
                    self.client_instance_id.clone(),
                    operation,
                    control,
                )],
                control,
            )
            .map_err(|error| transport_error("centralized-speech-recognizer", error))
    }
}
impl SpeechRecognizer for CentralizedSpeechRecognizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }
    fn transcribe(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
    ) -> Result<Voice2TextResponse> {
        let envelopes = self.invoke(request, control, false)?;
        let terminal = terminal_for(&envelopes)?;
        ensure_completed("centralized-speech-recognizer", terminal)?;
        let Some(TerminalResult::SpeechToText(result)) = terminal.result.as_ref() else {
            return Err(invalid("a speech-to-text terminal result"));
        };
        let segments = result
            .segments
            .iter()
            .map(|segment| {
                serde_json::from_slice(&segment.bytes)
                    .map_err(|_| invalid("JSON speech segment metadata"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Voice2TextResponse {
            transcript: result.transcript.clone(),
            language: result.language.clone(),
            confidence: result.confidence,
            segments,
            metadata: EngineMetadata {
                engine_id: "centralized-speech".into(),
                model: request.model.clone(),
                metadata: Value::Null,
            },
        })
    }
    fn stream_with_events(
        &self,
        request: &Voice2TextRequest,
        control: &InvocationControl,
        sink: &mut Voice2TextStreamEventSink<'_>,
    ) -> Result<()> {
        for envelope in self.invoke(request, control, true)? {
            match envelope.payload {
                Some(Payload::LlmStreamEvent(event)) => match event.event {
                    Some(
                        lumvise_resource_routing::protocol::llm_stream_event_v1::Event::TextDelta(
                            text,
                        ),
                    ) => sink(Voice2TextStreamEvent::TranscriptChunk {
                        sequence: event.event_sequence,
                        text,
                        is_final: false,
                    })?,
                    _ => return Err(invalid("a speech transcript stream event")),
                },
                Some(Payload::Terminal(terminal)) => {
                    ensure_completed("centralized-speech-recognizer", &terminal)?;
                    return sink(Voice2TextStreamEvent::Complete);
                }
                _ => return Err(invalid("speech stream frames followed by a terminal")),
            }
        }
        Err(invalid("a terminal speech-to-text stream frame"))
    }
}

impl CentralizedSpeechSynthesizer {
    pub fn new(
        client: Arc<dyn ResourceInvocationClient>,
        client_instance_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            client_instance_id: client_instance_id.into(),
        }
    }
    fn invoke(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        stream: bool,
    ) -> Result<Vec<InvocationEnvelopeV1>> {
        let op = TextToSpeechV1 {
            text: request.text.clone(),
            voice: request.voice_id.clone().unwrap_or_default(),
            model: request.model.clone(),
        };
        let operation = if stream {
            Operation::TextToSpeechStream(TextToSpeechStreamV1 { request: Some(op) })
        } else {
            Operation::TextToSpeech(op)
        };
        self.client
            .invoke(
                &[start_envelope(
                    Uuid::new_v4().to_string(),
                    self.client_instance_id.clone(),
                    operation,
                    control,
                )],
                control,
            )
            .map_err(|error| transport_error("centralized-speech-synthesizer", error))
    }
}
impl SpeechSynthesizer for CentralizedSpeechSynthesizer {
    fn warmup(&self) -> Result<()> {
        Ok(())
    }
    fn synthesize(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
    ) -> Result<Text2VoiceResponse> {
        let envelopes = self.invoke(request, control, false)?;
        let terminal = terminal_for(&envelopes)?;
        ensure_completed("centralized-speech-synthesizer", terminal)?;
        let Some(TerminalResult::TextToSpeech(result)) = terminal.result.as_ref() else {
            return Err(invalid("a text-to-speech terminal result"));
        };
        let metadata = result
            .metadata_json
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(|_| invalid("JSON text-to-speech metadata"))?
            .unwrap_or(Value::Null);
        Ok(Text2VoiceResponse {
            audio: result.audio.clone(),
            media_type: result.media_type.clone(),
            sample_rate_hz: result.sample_rate,
            metadata: EngineMetadata {
                engine_id: "centralized-speech".into(),
                model: request.model.clone(),
                metadata,
            },
        })
    }
    fn stream_with_events(
        &self,
        request: &Text2VoiceRequest,
        control: &InvocationControl,
        sink: &mut Text2VoiceStreamEventSink<'_>,
    ) -> Result<()> {
        for envelope in self.invoke(request, control, true)? {
            match envelope.payload {
                Some(Payload::LlmStreamEvent(event)) => match event.event {
                    Some(
                        lumvise_resource_routing::protocol::llm_stream_event_v1::Event::Binary(
                            chunk,
                        ),
                    ) => sink(Text2VoiceStreamEvent::AudioChunk {
                        sequence: event.event_sequence,
                        audio: chunk.bytes,
                        media_type: chunk.type_name,
                    })?,
                    _ => return Err(invalid("a binary text-to-speech stream event")),
                },
                Some(Payload::Terminal(terminal)) => {
                    ensure_completed("centralized-speech-synthesizer", &terminal)?;
                    return sink(Text2VoiceStreamEvent::Complete);
                }
                _ => return Err(invalid("speech stream frames followed by a terminal")),
            }
        }
        Err(invalid("a terminal text-to-speech stream frame"))
    }
}

fn start_envelope(
    request_id: String,
    client_instance_id: String,
    operation: Operation,
    control: &InvocationControl,
) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: lumvise_resource_routing::protocol::PROTOCOL_MAJOR,
        protocol_minor: lumvise_resource_routing::protocol::PROTOCOL_MINOR,
        request_id,
        sequence: 0,
        payload: Some(Payload::Start(
            lumvise_resource_routing::protocol::InvocationStartV1 {
                deadline_unix_ms: control.deadline_unix_ms(),
                client_instance_id,
                operation: Some(operation),
            },
        )),
    }
}
fn encode_llm_input(
    messages: &[LlmMessage],
    modalities: &[super::LlmModalityInput],
) -> Result<Vec<TypedBinaryChunkV1>> {
    let mut result = messages
        .iter()
        .map(|message| TypedBinaryChunkV1 {
            type_name: "lumvise.llm.message.v1".into(),
            bytes: message.content.as_bytes().to_vec(),
            metadata_json: Some(
                serde_json::to_vec(&json!({"role": message.role})).expect("fixed JSON"),
            ),
            final_chunk: true,
        })
        .collect::<Vec<_>>();
    result.extend(modalities.iter().map(|input| TypedBinaryChunkV1 { type_name: format!("lumvise.llm.modality.{}", input.input_id), bytes: input.bytes.clone(), metadata_json: Some(serde_json::to_vec(&json!({"kind": input.kind, "media_type": input.media_type, "metadata": input.metadata})).expect("serializable modality metadata")), final_chunk: true }));
    Ok(result)
}
fn terminal_for(
    envelopes: &[InvocationEnvelopeV1],
) -> Result<&lumvise_resource_routing::protocol::InvocationTerminalV1> {
    envelopes
        .last()
        .and_then(|envelope| match &envelope.payload {
            Some(Payload::Terminal(terminal)) => Some(terminal),
            _ => None,
        })
        .ok_or_else(|| invalid("one terminal invocation frame"))
}
fn ensure_completed(
    provider: &str,
    terminal: &lumvise_resource_routing::protocol::InvocationTerminalV1,
) -> Result<()> {
    if terminal.status == InvocationTerminalStatusV1::Completed as i32 {
        Ok(())
    } else {
        Err(NeuralError::ProviderFailed {
            provider_id: provider.into(),
            message: terminal
                .message
                .clone()
                .unwrap_or_else(|| format!("central terminal status {}", terminal.status)),
        })
    }
}
fn transport_error(provider: &str, error: lumvise_resource_routing::TransportError) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: provider.into(),
        message: error.to_string(),
    }
}
fn control_error(provider: &str) -> NeuralError {
    NeuralError::ProviderFailed {
        provider_id: provider.into(),
        message: "invocation cancelled or deadline elapsed".into(),
    }
}
fn invalid(expected: &str) -> NeuralError {
    NeuralError::InvalidValue {
        value: "centralized resource protocol".into(),
        expected: expected.into(),
    }
}
fn is_loopback_url(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| host == "127.0.0.1" || host == "::1" || host == "localhost")
}
