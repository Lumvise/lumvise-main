use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use lumvise_neural_core::{
    LlmProviderRegistry, SpeechRecognizer, SpeechSynthesizer,
    llm_providers::{LlmExecutionControl, LlmExecutorRegistry},
    text2voice::Text2VoiceRequest,
    voice2text::Voice2TextRequest,
};
use lumvise_resource_routing::{
    InvocationControl,
    auth::AuthenticatedPrincipal,
    protocol::{
        CapabilityReadinessEntryV1, CapabilityReadinessV1, InvocationEnvelopeV1, InvocationStartV1,
        InvocationTerminalStatusV1, InvocationTerminalV1, LlmCompleteV1, LlmProviderDescriptorV1,
        LlmResultV1, LlmStreamEventV1, PROTOCOL_MINOR, PersistenceResultV1, ReadinessRequestV1,
        ReadinessResponseV1, ResourceCapabilityV1, SequenceValidator, SpeechDescriptorV1,
        SpeechToTextResultV1, TextToSpeechResultV1, TypedBinaryChunkV1,
        invocation_envelope_v1::Payload, invocation_start_v1::Operation,
        invocation_terminal_v1::Result as TerminalResult,
        llm_stream_event_v1::Event as StreamEvent,
    },
};
use thiserror::Error;
use uuid::Uuid;

use crate::tenant::{TenantAdapterCache, TenantKey, TenantOpenError};

/// The protocol-to-owned-persistence seam belongs to the server, not to DB
/// Core. It prevents HTTP/Protobuf shapes from leaking into persistence
/// interfaces and lets the future centralized client share an exhaustive codec.
pub trait PersistenceProtocolCodec: Send + Sync {
    fn decode_semantic(
        &self,
        operation: &lumvise_resource_routing::protocol::SemanticOperationV1,
    ) -> Result<lumvise_db_core::SemanticOperation, String>;
    fn encode_semantic(
        &self,
        result: lumvise_db_core::SemanticResult,
    ) -> Result<PersistenceResultV1, String>;
    fn decode_relational(
        &self,
        operation: &lumvise_resource_routing::protocol::RelationalOperationV1,
    ) -> Result<lumvise_db_core::RelationalOperation, String>;
    fn encode_relational(
        &self,
        result: lumvise_db_core::RelationalResult,
    ) -> Result<PersistenceResultV1, String>;
}

/// Shared binary codec for the DB Core's exhaustive owned persistence enums.
/// It validates the declared enum name against the decoded binary payload, so
/// a request cannot smuggle one persistence operation under another name.
#[derive(Default)]
pub struct CentralizedPersistenceProtocolCodec;

impl PersistenceProtocolCodec for CentralizedPersistenceProtocolCodec {
    fn decode_semantic(
        &self,
        operation: &lumvise_resource_routing::protocol::SemanticOperationV1,
    ) -> Result<lumvise_db_core::SemanticOperation, String> {
        lumvise_db_core::CentralizedPersistence::decode_semantic_operation(operation)
            .map_err(|error| error.to_string())
    }
    fn encode_semantic(
        &self,
        result: lumvise_db_core::SemanticResult,
    ) -> Result<PersistenceResultV1, String> {
        lumvise_db_core::CentralizedPersistence::encode_semantic_result(result)
            .map_err(|error| error.to_string())
    }
    fn decode_relational(
        &self,
        operation: &lumvise_resource_routing::protocol::RelationalOperationV1,
    ) -> Result<lumvise_db_core::RelationalOperation, String> {
        lumvise_db_core::CentralizedPersistence::decode_relational_operation(operation)
            .map_err(|error| error.to_string())
    }
    fn encode_relational(
        &self,
        result: lumvise_db_core::RelationalResult,
    ) -> Result<PersistenceResultV1, String> {
        lumvise_db_core::CentralizedPersistence::encode_relational_result(result)
            .map_err(|error| error.to_string())
    }
}

pub struct DispatchResources {
    pub llm_registry: Option<Arc<Mutex<LlmProviderRegistry>>>,
    pub speech_recognizer: Option<Arc<dyn SpeechRecognizer>>,
    pub speech_synthesizer: Option<Arc<dyn SpeechSynthesizer>>,
    pub tenants: Arc<TenantAdapterCache>,
    pub persistence_codec: Arc<dyn PersistenceProtocolCodec>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ActiveInvocationKey {
    pub issuer: String,
    pub tenant_id: String,
    pub subject: String,
    pub client_instance_id: String,
    pub request_id: String,
}

pub struct ResourceDispatcher {
    resources: DispatchResources,
    llm_executor: Option<Arc<LlmExecutorRegistry>>,
    active: Mutex<HashMap<ActiveInvocationKey, InvocationControl>>,
    server_instance_id: String,
}

impl ResourceDispatcher {
    pub fn new(resources: DispatchResources) -> Self {
        let llm_executor = resources
            .llm_registry
            .as_ref()
            .map(|registry| LlmExecutorRegistry::new(Arc::clone(registry)));
        Self {
            resources,

            llm_executor,
            active: Mutex::new(HashMap::new()),
            server_instance_id: Uuid::new_v4().to_string(),
        }
    }

    pub fn tenant_database_path(&self, principal: &AuthenticatedPrincipal) -> std::path::PathBuf {
        self.resources
            .tenants
            .database_path(&TenantKey::from(principal))
    }

    pub fn server_instance_id(&self) -> &str {
        &self.server_instance_id
    }

    pub fn cancel(
        &self,
        principal: &AuthenticatedPrincipal,
        client_instance_id: &str,
        request_id: &str,
    ) -> bool {
        let key = ActiveInvocationKey {
            issuer: principal.issuer.clone(),
            tenant_id: principal.tenant_id.clone(),
            subject: principal.subject.clone(),
            client_instance_id: client_instance_id.into(),
            request_id: request_id.into(),
        };
        if let Some(control) = self
            .active
            .lock()
            .expect("active invocation mutex poisoned")
            .get(&key)
        {
            control.cancel();
            true
        } else {
            false
        }
    }

    fn llm_is_configured(&self) -> bool {
        self.resources
            .llm_registry
            .as_ref()
            .is_some_and(|registry| {
                !registry
                    .lock()
                    .expect("LLM registry mutex poisoned")
                    .provider_ids()
                    .is_empty()
            })
    }

    pub fn readiness(
        &self,
        principal: &AuthenticatedPrincipal,
        request: ReadinessRequestV1,
    ) -> Result<ReadinessResponseV1, DispatchError> {
        validate_deadline(request.deadline_unix_ms)?;
        if !request
            .supported_majors
            .contains(&lumvise_resource_routing::protocol::PROTOCOL_MAJOR)
        {
            return Err(DispatchError::UnsupportedMajor);
        }
        if !request.supported_minors.contains(&PROTOCOL_MINOR) {
            return Err(DispatchError::UnsupportedMinor);
        }
        let requires_persistence = request.requested_capabilities.iter().any(|capability| {
            matches!(
                ResourceCapabilityV1::try_from(*capability),
                Ok(ResourceCapabilityV1::GraphPersistence | ResourceCapabilityV1::SqlPersistence)
            )
        });
        let tenant = requires_persistence
            .then(|| self.tenant(&TenantKey::from(principal)))
            .transpose()?;
        let semantic_ready = tenant
            .as_ref()
            .map(|tenant| {
                tenant
                    .semantic
                    .readiness()
                    .map_err(|error| DispatchError::Adapter(error.to_string()))
            })
            .transpose()?;
        let relational_ready = tenant
            .as_ref()
            .map(|tenant| {
                tenant
                    .relational
                    .readiness()
                    .map_err(|error| DispatchError::Adapter(error.to_string()))
            })
            .transpose()?;
        let mut capabilities = Vec::new();
        for capability in request.requested_capabilities {
            let status = match ResourceCapabilityV1::try_from(capability).ok() {
                Some(ResourceCapabilityV1::LlmExecution) if self.llm_is_configured() => {
                    CapabilityReadinessV1::Ready
                }
                Some(ResourceCapabilityV1::SpeechInference)
                    if self.resources.speech_recognizer.is_some()
                        && self.resources.speech_synthesizer.is_some() =>
                {
                    CapabilityReadinessV1::Ready
                }
                Some(ResourceCapabilityV1::GraphPersistence)
                    if semantic_ready.as_ref().is_some_and(|ready| ready.ready) =>
                {
                    CapabilityReadinessV1::Ready
                }
                Some(ResourceCapabilityV1::SqlPersistence)
                    if relational_ready.as_ref().is_some_and(|ready| ready.ready) =>
                {
                    CapabilityReadinessV1::Ready
                }
                Some(
                    ResourceCapabilityV1::LlmExecution | ResourceCapabilityV1::SpeechInference,
                ) => CapabilityReadinessV1::NotConfigured,
                Some(
                    ResourceCapabilityV1::GraphPersistence | ResourceCapabilityV1::SqlPersistence,
                ) => CapabilityReadinessV1::Recovering,
                None => return Err(DispatchError::UnknownCapability(capability)),
            };
            capabilities.push(CapabilityReadinessEntryV1 {
                capability,
                status: status as i32,
            });
        }
        let llm_providers =
            self.resources
                .llm_registry
                .as_ref()
                .map_or_else(Vec::new, |registry| {
                    let registry = registry.lock().expect("LLM registry mutex poisoned");
                    registry
                        .provider_ids()
                        .into_iter()
                        .map(|provider_id| {
                            let capabilities = registry
                                .capabilities(&provider_id)
                                .map(|capabilities| {
                                    serde_json::to_vec(&capabilities).unwrap_or_default()
                                })
                                .unwrap_or_default();
                            LlmProviderDescriptorV1 {
                                concurrency: registry.completion_concurrency(&provider_id) as u32,
                                provider_id,
                                capabilities,
                            }
                        })
                        .collect()
                });
        let mut speech = Vec::new();
        if self.resources.speech_recognizer.is_some() && self.resources.speech_synthesizer.is_some()
        {
            speech.push(SpeechDescriptorV1 {
                adapter_id: "internal-speech".into(),
                models: Vec::new(),
            });
        }
        Ok(ReadinessResponseV1 {
            tenant_id: principal.tenant_id.clone(),
            server_instance_id: self.server_instance_id.clone(),
            negotiated_minor: PROTOCOL_MINOR,
            capabilities,
            llm_providers,
            speech,
        })
    }
    pub fn invoke(
        &self,
        principal: &AuthenticatedPrincipal,
        envelopes: Vec<InvocationEnvelopeV1>,
    ) -> InvocationEnvelopeV1 {
        self.invoke_envelopes(principal, envelopes)
            .pop()
            .expect("resource invocation always returns a terminal envelope")
    }

    pub fn invoke_envelopes(
        &self,
        principal: &AuthenticatedPrincipal,
        envelopes: Vec<InvocationEnvelopeV1>,
    ) -> Vec<InvocationEnvelopeV1> {
        match self.invoke_inner(principal, envelopes) {
            Ok(envelopes) => envelopes,
            Err(error) => vec![response_envelope(String::new(), terminal_from_error(error))],
        }
    }

    fn invoke_inner(
        &self,
        principal: &AuthenticatedPrincipal,
        envelopes: Vec<InvocationEnvelopeV1>,
    ) -> Result<Vec<InvocationEnvelopeV1>, DispatchError> {
        let first = envelopes.first().ok_or(DispatchError::MissingStart)?;
        if first.request_id.trim().is_empty() {
            return Err(DispatchError::Invalid("request ID is blank".into()));
        }
        let request_id = first.request_id.clone();
        let mut sequence = SequenceValidator::new(&request_id);
        for envelope in &envelopes {
            sequence
                .validate(envelope)
                .map_err(|error| DispatchError::Protocol(error.to_string()))?;
            if envelope.protocol_minor != PROTOCOL_MINOR {
                return Err(DispatchError::UnsupportedMinor);
            }
        }
        let start = match first.payload.as_ref() {
            Some(Payload::Start(start)) => start,
            _ => return Err(DispatchError::MissingStart),
        };
        if envelopes
            .iter()
            .skip(1)
            .any(|envelope| matches!(envelope.payload, Some(Payload::Start(_))))
        {
            return Err(DispatchError::Invalid(
                "invocation stream has more than one start frame".into(),
            ));
        }
        validate_deadline(start.deadline_unix_ms)?;
        if start.client_instance_id.trim().is_empty() {
            return Err(DispatchError::Invalid("client instance ID is blank".into()));
        }
        let control = InvocationControl::from_deadline_unix_ms(start.deadline_unix_ms);
        if envelopes
            .iter()
            .any(|envelope| matches!(envelope.payload, Some(Payload::Cancel(_))))
        {
            control.cancel();
        }
        let key = ActiveInvocationKey {
            issuer: principal.issuer.clone(),
            tenant_id: principal.tenant_id.clone(),
            subject: principal.subject.clone(),
            client_instance_id: start.client_instance_id.clone(),
            request_id,
        };
        self.active
            .lock()
            .expect("active invocation mutex poisoned")
            .insert(key.clone(), control.clone());
        let result = self.dispatch_operation(principal, start, &control, &key.request_id);
        self.active
            .lock()
            .expect("active invocation mutex poisoned")
            .remove(&key);
        result
    }

    fn dispatch_operation(
        &self,
        principal: &AuthenticatedPrincipal,
        start: &InvocationStartV1,
        control: &InvocationControl,
        request_id: &str,
    ) -> Result<Vec<InvocationEnvelopeV1>, DispatchError> {
        if control.is_cancelled() {
            return Ok(vec![response_envelope(
                request_id.into(),
                terminal(
                    InvocationTerminalStatusV1::Cancelled,
                    Some("cancelled".into()),
                    None,
                ),
            )]);
        }
        if control.is_expired() {
            return Ok(vec![response_envelope(
                request_id.into(),
                terminal(
                    InvocationTerminalStatusV1::DeadlineExceeded,
                    Some("deadline exceeded".into()),
                    None,
                ),
            )]);
        }
        match start.operation.as_ref() {
            Some(Operation::SpeechToTextStream(operation)) => {
                return self.transcribe_stream(operation, control, request_id);
            }
            Some(Operation::TextToSpeechStream(operation)) => {
                return self.synthesize_stream(operation, control, request_id);
            }
            _ => {}
        }
        let terminal = match start
            .operation
            .as_ref()
            .ok_or(DispatchError::MissingOperation)?
        {
            Operation::Readiness(_) => {
                return Err(DispatchError::Invalid(
                    "readiness is only valid on /resources/v1/readiness".into(),
                ));
            }
            Operation::SemanticOperation(operation) => {
                let tenant = self.tenant(&TenantKey::from(principal))?;
                let owned = self
                    .resources
                    .persistence_codec
                    .decode_semantic(operation)
                    .map_err(DispatchError::Invalid)?;
                let result = tenant
                    .semantic
                    .execute(owned, control)
                    .map_err(|error| DispatchError::Adapter(error.to_string()))?;
                let result = self
                    .resources
                    .persistence_codec
                    .encode_semantic(result)
                    .map_err(DispatchError::Internal)?;
                terminal(
                    InvocationTerminalStatusV1::Completed,
                    None,
                    Some(TerminalResult::Semantic(result)),
                )
            }
            Operation::RelationalOperation(operation) => {
                let tenant = self.tenant(&TenantKey::from(principal))?;
                let owned = self
                    .resources
                    .persistence_codec
                    .decode_relational(operation)
                    .map_err(DispatchError::Invalid)?;
                let result = tenant
                    .relational
                    .execute(owned, control)
                    .map_err(|error| DispatchError::Adapter(error.to_string()))?;
                let result = self
                    .resources
                    .persistence_codec
                    .encode_relational(result)
                    .map_err(DispatchError::Internal)?;
                terminal(
                    InvocationTerminalStatusV1::Completed,
                    None,
                    Some(TerminalResult::Relational(result)),
                )
            }
            Operation::SpeechToText(operation) => self.transcribe(operation, control)?,
            Operation::TextToSpeech(operation) => self.synthesize(operation, control)?,
            Operation::LlmComplete(operation) => self.complete_llm(operation, control)?,
            Operation::LlmStream(_)
            | Operation::SpeechToTextStream(_)
            | Operation::TextToSpeechStream(_) => {
                return Err(DispatchError::UnsupportedStreamOperation);
            }
        };
        Ok(vec![response_envelope(request_id.into(), terminal)])
    }

    fn tenant(&self, key: &TenantKey) -> Result<Arc<crate::tenant::TenantAdapters>, DispatchError> {
        self.resources
            .tenants
            .get_or_open(key)
            .map_err(DispatchError::Tenant)
    }

    fn transcribe(
        &self,
        operation: &lumvise_resource_routing::protocol::SpeechToTextV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, DispatchError> {
        let adapter = self
            .resources
            .speech_recognizer
            .as_ref()
            .ok_or(DispatchError::NotConfigured("speech recognition"))?;
        let response = adapter
            .transcribe(
                &Voice2TextRequest {
                    audio: operation.audio.clone(),
                    media_type: operation.media_type.clone(),
                    model: operation.model.clone(),
                },
                control,
            )
            .map_err(|error| DispatchError::Adapter(error.to_string()))?;
        let segments = response
            .segments
            .into_iter()
            .map(|segment| TypedBinaryChunkV1 {
                type_name: "serde_json::Value".into(),
                bytes: serde_json::to_vec(&segment).unwrap_or_default(),
                metadata_json: None,
                final_chunk: true,
            })
            .collect();
        Ok(terminal(
            InvocationTerminalStatusV1::Completed,
            None,
            Some(TerminalResult::SpeechToText(SpeechToTextResultV1 {
                transcript: response.transcript,
                language: response.language,
                confidence: response.confidence,
                segments,
            })),
        ))
    }

    fn synthesize(
        &self,
        operation: &lumvise_resource_routing::protocol::TextToSpeechV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, DispatchError> {
        let adapter = self
            .resources
            .speech_synthesizer
            .as_ref()
            .ok_or(DispatchError::NotConfigured("speech synthesis"))?;
        let response = adapter
            .synthesize(
                &Text2VoiceRequest {
                    text: operation.text.clone(),
                    voice_id: (!operation.voice.is_empty()).then(|| operation.voice.clone()),
                    model: operation.model.clone(),
                },
                control,
            )
            .map_err(|error| DispatchError::Adapter(error.to_string()))?;
        Ok(terminal(
            InvocationTerminalStatusV1::Completed,
            None,
            Some(TerminalResult::TextToSpeech(TextToSpeechResultV1 {
                audio: response.audio,
                media_type: response.media_type,
                sample_rate: response.sample_rate_hz,
                metadata_json: serde_json::to_vec(&response.metadata).ok(),
            })),
        ))
    }
    fn transcribe_stream(
        &self,
        operation: &lumvise_resource_routing::protocol::SpeechToTextStreamV1,
        control: &InvocationControl,
        request_id: &str,
    ) -> Result<Vec<InvocationEnvelopeV1>, DispatchError> {
        let request = operation
            .request
            .as_ref()
            .ok_or_else(|| DispatchError::Invalid("speech stream request is required".into()))?;
        let adapter = self
            .resources
            .speech_recognizer
            .as_ref()
            .ok_or(DispatchError::NotConfigured("speech recognition"))?;
        let mut output = Vec::new();
        adapter
            .stream_with_events(
                &Voice2TextRequest {
                    audio: request.audio.clone(),
                    media_type: request.media_type.clone(),
                    model: request.model.clone(),
                },
                control,
                &mut |event| match event {
                    lumvise_neural_core::voice2text::Voice2TextStreamEvent::TranscriptChunk {
                        sequence,
                        text,
                        ..
                    } => {
                        output.push(stream_envelope(
                            request_id,
                            sequence,
                            StreamEvent::TextDelta(text),
                        ));
                        Ok(())
                    }
                    lumvise_neural_core::voice2text::Voice2TextStreamEvent::Complete => Ok(()),
                    lumvise_neural_core::voice2text::Voice2TextStreamEvent::Error { message } => {
                        Err(lumvise_neural_core::NeuralError::ProviderFailed {
                            provider_id: "speech-to-text-stream".into(),
                            message,
                        })
                    }
                },
            )
            .map_err(|error| DispatchError::Adapter(error.to_string()))?;
        output.push(response_envelope(
            request_id.into(),
            terminal(InvocationTerminalStatusV1::Completed, None, None),
        ));
        Ok(output)
    }

    fn synthesize_stream(
        &self,
        operation: &lumvise_resource_routing::protocol::TextToSpeechStreamV1,
        control: &InvocationControl,
        request_id: &str,
    ) -> Result<Vec<InvocationEnvelopeV1>, DispatchError> {
        let request = operation
            .request
            .as_ref()
            .ok_or_else(|| DispatchError::Invalid("speech stream request is required".into()))?;
        let adapter = self
            .resources
            .speech_synthesizer
            .as_ref()
            .ok_or(DispatchError::NotConfigured("speech synthesis"))?;
        let mut output = Vec::new();
        adapter
            .stream_with_events(
                &Text2VoiceRequest {
                    text: request.text.clone(),
                    voice_id: (!request.voice.is_empty()).then(|| request.voice.clone()),
                    model: request.model.clone(),
                },
                control,
                &mut |event| match event {
                    lumvise_neural_core::text2voice::Text2VoiceStreamEvent::AudioChunk {
                        sequence,
                        audio,
                        media_type,
                    } => {
                        output.push(stream_envelope(
                            request_id,
                            sequence,
                            StreamEvent::Binary(TypedBinaryChunkV1 {
                                type_name: media_type,
                                bytes: audio,
                                metadata_json: None,
                                final_chunk: false,
                            }),
                        ));
                        Ok(())
                    }
                    lumvise_neural_core::text2voice::Text2VoiceStreamEvent::Complete => Ok(()),
                    lumvise_neural_core::text2voice::Text2VoiceStreamEvent::Error { message } => {
                        Err(lumvise_neural_core::NeuralError::ProviderFailed {
                            provider_id: "text-to-speech-stream".into(),
                            message,
                        })
                    }
                },
            )
            .map_err(|error| DispatchError::Adapter(error.to_string()))?;
        output.push(response_envelope(
            request_id.into(),
            terminal(InvocationTerminalStatusV1::Completed, None, None),
        ));
        Ok(output)
    }

    fn complete_llm(
        &self,
        operation: &LlmCompleteV1,
        control: &InvocationControl,
    ) -> Result<InvocationTerminalV1, DispatchError> {
        let executor = self
            .llm_executor
            .as_ref()
            .ok_or(DispatchError::NotConfigured("LLM execution"))?;
        let request = decode_llm_request(operation)?;
        let provider_id = request
            .provider_id
            .clone()
            .ok_or_else(|| DispatchError::Invalid("LLM provider ID is blank".into()))?;
        let response = executor
            .complete(
                &provider_id,
                request,
                LlmExecutionControl::new(control.child()),
            )
            .map_err(|error| DispatchError::Adapter(error.to_string()))?;
        let metadata = serde_json::to_vec(&response.metadata)
            .map_err(|error| DispatchError::Internal(error.to_string()))?;
        Ok(terminal(
            InvocationTerminalStatusV1::Completed,
            None,
            Some(TerminalResult::Llm(LlmResultV1 {
                provider_session_id: operation.provider_session_id.clone(),
                output: vec![TypedBinaryChunkV1 {
                    type_name: "lumvise.llm.text.v1".into(),
                    bytes: response.content.into_bytes(),
                    metadata_json: Some(metadata),
                    final_chunk: true,
                }],
            })),
        ))
    }
}

fn decode_llm_request(
    operation: &LlmCompleteV1,
) -> Result<lumvise_neural_core::llm_providers::LlmRequest, DispatchError> {
    let messages = operation
        .input
        .iter()
        .map(|chunk| {
            if chunk.type_name != "lumvise.llm.message.v1" {
                return Err(DispatchError::Invalid(format!(
                    "unsupported LLM input type {:?}",
                    chunk.type_name
                )));
            }
            let role = chunk
                .metadata_json
                .as_deref()
                .map(|bytes| {
                    serde_json::from_slice::<serde_json::Value>(bytes)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("role")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                })
                .flatten()
                .ok_or_else(|| {
                    DispatchError::Invalid("LLM message is missing UTF-8 JSON role metadata".into())
                })?;
            let content = String::from_utf8(chunk.bytes.clone())
                .map_err(|_| DispatchError::Invalid("LLM message body is not UTF-8".into()))?;
            Ok(lumvise_neural_core::llm_providers::LlmMessage { role, content })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(lumvise_neural_core::llm_providers::LlmRequest {
        options: Default::default(),
        messages,
        stream: false,
        provider_id: Some(operation.provider_id.clone()),
        model: operation.model_override.clone(),
        conversation_id: operation.conversation_id.clone(),
        provider_session_id: operation.provider_session_id.clone(),
        mcp_servers: Vec::new(),
        modality_inputs: Vec::new(),
    })
}

fn validate_deadline(deadline_unix_ms: u64) -> Result<(), DispatchError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock predates Unix epoch")
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    if deadline_unix_ms <= now {
        return Err(DispatchError::DeadlineExceeded);
    }
    if deadline_unix_ms
        > now.saturating_add(
            InvocationControl::DEADLINE
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        )
    {
        return Err(DispatchError::Invalid(
            "deadline exceeds the one 60-second invocation budget".into(),
        ));
    }
    Ok(())
}

fn stream_envelope(request_id: &str, sequence: u64, event: StreamEvent) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: lumvise_resource_routing::protocol::PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id: request_id.into(),
        sequence,
        payload: Some(Payload::LlmStreamEvent(LlmStreamEventV1 {
            event_sequence: sequence,
            event: Some(event),
        })),
    }
}

fn response_envelope(request_id: String, terminal: InvocationTerminalV1) -> InvocationEnvelopeV1 {
    InvocationEnvelopeV1 {
        protocol_major: lumvise_resource_routing::protocol::PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id,
        sequence: 0,
        payload: Some(Payload::Terminal(terminal)),
    }
}

fn terminal(
    status: InvocationTerminalStatusV1,
    message: Option<String>,
    result: Option<TerminalResult>,
) -> InvocationTerminalV1 {
    InvocationTerminalV1 {
        status: status as i32,
        retryable: matches!(
            status,
            InvocationTerminalStatusV1::Busy | InvocationTerminalStatusV1::Unavailable
        ),
        outcome_unknown: false,
        error_code: None,
        message,
        result,
    }
}

fn terminal_from_error(error: DispatchError) -> InvocationTerminalV1 {
    let (status, retryable) = match error {
        DispatchError::DeadlineExceeded => (InvocationTerminalStatusV1::DeadlineExceeded, false),
        DispatchError::UnsupportedMajor
        | DispatchError::UnsupportedMinor
        | DispatchError::MissingStart
        | DispatchError::MissingOperation
        | DispatchError::Protocol(_)
        | DispatchError::Invalid(_)
        | DispatchError::UnknownCapability(_)
        | DispatchError::UnsupportedStreamOperation => {
            (InvocationTerminalStatusV1::InvalidArgument, false)
        }
        DispatchError::NotConfigured(_) | DispatchError::Tenant(_) => {
            (InvocationTerminalStatusV1::Unavailable, true)
        }
        DispatchError::Adapter(_) | DispatchError::Internal(_) => {
            (InvocationTerminalStatusV1::Failed, false)
        }
    };
    InvocationTerminalV1 {
        status: status as i32,
        retryable,
        outcome_unknown: false,
        error_code: Some(error.code().into()),
        message: Some(error.to_string()),
        result: None,
    }
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("invocation deadline elapsed")]
    DeadlineExceeded,
    #[error("protocol validation failed: {0}")]
    Protocol(String),
    #[error("invocation start frame is required")]
    MissingStart,
    #[error("invocation start has no operation")]
    MissingOperation,
    #[error("protocol major is unsupported")]
    UnsupportedMajor,
    #[error("protocol minor is unsupported")]
    UnsupportedMinor,
    #[error("unknown readiness capability {0}")]
    UnknownCapability(i32),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("central server has no configured {0} adapter")]
    NotConfigured(&'static str),
    #[error("opening tenant adapters failed: {0}")]
    Tenant(#[from] TenantOpenError),
    #[error("owning adapter failed: {0}")]
    Adapter(String),
    #[error("central server persistence codec failed: {0}")]
    Internal(String),
    #[error("streaming operation has no server dispatch implementation")]
    UnsupportedStreamOperation,
}

impl DispatchError {
    fn code(&self) -> &'static str {
        match self {
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Protocol(_) => "protocol_error",
            Self::MissingStart => "missing_start",
            Self::MissingOperation => "missing_operation",
            Self::UnsupportedMajor => "unsupported_major",
            Self::UnsupportedMinor => "unsupported_minor",
            Self::UnknownCapability(_) => "unknown_capability",
            Self::Invalid(_) => "invalid_argument",
            Self::NotConfigured(_) => "not_configured",
            Self::Tenant(_) => "tenant_unavailable",
            Self::Adapter(_) => "adapter_failed",
            Self::Internal(_) => "internal",
            Self::UnsupportedStreamOperation => "unsupported_stream_operation",
        }
    }
}
