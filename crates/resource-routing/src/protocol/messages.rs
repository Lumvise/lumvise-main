use prost::{Enumeration, Message, Oneof};

#[derive(Clone, PartialEq, Message)]
pub struct InvocationEnvelopeV1 {
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    #[prost(uint32, tag = "2")]
    pub protocol_minor: u32,
    #[prost(string, tag = "3")]
    pub request_id: String,
    #[prost(uint64, tag = "4")]
    pub sequence: u64,
    #[prost(
        oneof = "invocation_envelope_v1::Payload",
        tags = "10, 11, 12, 13, 14, 15, 16"
    )]
    pub payload: Option<invocation_envelope_v1::Payload>,
}

pub mod invocation_envelope_v1 {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum Payload {
        #[prost(message, tag = "10")]
        Start(InvocationStartV1),
        #[prost(message, tag = "11")]
        BinaryChunk(TypedBinaryChunkV1),
        #[prost(message, tag = "12")]
        LlmStreamEvent(LlmStreamEventV1),
        #[prost(message, tag = "13")]
        ReverseMcpCall(ReverseMcpCallV1),
        #[prost(message, tag = "14")]
        ReverseMcpResult(ReverseMcpResultV1),
        #[prost(message, tag = "15")]
        Cancel(InvocationCancelV1),
        #[prost(message, tag = "16")]
        Terminal(InvocationTerminalV1),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct InvocationStartV1 {
    #[prost(uint64, tag = "1")]
    pub deadline_unix_ms: u64,
    #[prost(string, tag = "2")]
    pub client_instance_id: String,
    #[prost(
        oneof = "invocation_start_v1::Operation",
        tags = "10, 11, 12, 13, 14, 15, 16, 17, 19"
    )]
    pub operation: Option<invocation_start_v1::Operation>,
}

pub mod invocation_start_v1 {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum Operation {
        #[prost(message, tag = "10")]
        LlmComplete(LlmCompleteV1),
        #[prost(message, tag = "11")]
        LlmStream(LlmStreamV1),
        #[prost(message, tag = "12")]
        SpeechToText(SpeechToTextV1),
        #[prost(message, tag = "13")]
        SpeechToTextStream(SpeechToTextStreamV1),
        #[prost(message, tag = "14")]
        TextToSpeech(TextToSpeechV1),
        #[prost(message, tag = "15")]
        TextToSpeechStream(TextToSpeechStreamV1),
        #[prost(message, tag = "16")]
        SemanticOperation(SemanticOperationV1),
        #[prost(message, tag = "17")]
        RelationalOperation(RelationalOperationV1),
        #[prost(message, tag = "19")]
        Readiness(ReadinessRequestV1),
    }
}

/// A typed payload remains binary all the way to the owning adapter. `metadata_json`
/// is permitted only for existing intentional `serde_json::Value` metadata.
#[derive(Clone, PartialEq, Message)]
pub struct TypedBinaryChunkV1 {
    #[prost(string, tag = "1")]
    pub type_name: String,
    #[prost(bytes = "vec", tag = "2")]
    pub bytes: Vec<u8>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub metadata_json: Option<Vec<u8>>,
    #[prost(bool, tag = "4")]
    pub final_chunk: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct LlmCompleteV1 {
    #[prost(string, tag = "1")]
    pub provider_id: String,
    #[prost(string, optional, tag = "2")]
    pub model_override: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub conversation_id: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub provider_session_id: Option<String>,
    #[prost(message, repeated, tag = "5")]
    pub input: Vec<TypedBinaryChunkV1>,
    #[prost(string, repeated, tag = "6")]
    pub scoped_mcp_route_ids: Vec<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct LlmStreamV1 {
    #[prost(message, optional, tag = "1")]
    pub request: Option<LlmCompleteV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpeechToTextV1 {
    #[prost(bytes = "vec", tag = "1")]
    pub audio: Vec<u8>,
    #[prost(string, tag = "2")]
    pub media_type: String,
    #[prost(string, optional, tag = "3")]
    pub model: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpeechToTextStreamV1 {
    #[prost(message, optional, tag = "1")]
    pub request: Option<SpeechToTextV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TextToSpeechV1 {
    #[prost(string, tag = "1")]
    pub text: String,
    #[prost(string, tag = "2")]
    pub voice: String,
    #[prost(string, optional, tag = "3")]
    pub model: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TextToSpeechStreamV1 {
    #[prost(message, optional, tag = "1")]
    pub request: Option<TextToSpeechV1>,
}

/// The operation name is the exhaustive owning-module enum variant name; its
/// Prost payload is typed by that name and never JSON/base64 or raw SQL.
#[derive(Clone, PartialEq, Message)]
pub struct SemanticOperationV1 {
    #[prost(string, tag = "1")]
    pub operation_name: String,
    #[prost(message, repeated, tag = "2")]
    pub records: Vec<TypedBinaryChunkV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RelationalOperationV1 {
    #[prost(string, tag = "1")]
    pub operation_name: String,
    #[prost(message, repeated, tag = "2")]
    pub records: Vec<TypedBinaryChunkV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct LlmStreamEventV1 {
    #[prost(uint64, tag = "1")]
    pub event_sequence: u64,
    #[prost(oneof = "llm_stream_event_v1::Event", tags = "10, 11, 12")]
    pub event: Option<llm_stream_event_v1::Event>,
}

pub mod llm_stream_event_v1 {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum Event {
        #[prost(string, tag = "10")]
        TextDelta(String),
        #[prost(message, tag = "11")]
        ToolCall(ReverseMcpCallV1),
        #[prost(message, tag = "12")]
        Binary(TypedBinaryChunkV1),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct ReverseMcpCallV1 {
    #[prost(string, tag = "1")]
    pub route_id: String,
    #[prost(string, tag = "2")]
    pub method: String,
    #[prost(message, repeated, tag = "3")]
    pub parameters: Vec<TypedBinaryChunkV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ReverseMcpResultV1 {
    #[prost(string, tag = "1")]
    pub route_id: String,
    #[prost(message, repeated, tag = "2")]
    pub result: Vec<TypedBinaryChunkV1>,
    #[prost(string, optional, tag = "3")]
    pub error_code: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct InvocationCancelV1 {
    #[prost(string, tag = "2")]
    pub client_instance_id: String,
    #[prost(string, optional, tag = "1")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Enumeration)]
#[repr(i32)]
pub enum InvocationTerminalStatusV1 {
    Completed = 0,
    InvalidArgument = 1,
    Busy = 2,
    DeadlineExceeded = 3,
    Cancelled = 4,
    Unavailable = 5,
    Conflict = 6,
    Failed = 7,
    Internal = 8,
}

#[derive(Clone, PartialEq, Message)]
pub struct InvocationTerminalV1 {
    #[prost(enumeration = "InvocationTerminalStatusV1", tag = "1")]
    pub status: i32,
    #[prost(bool, tag = "2")]
    pub retryable: bool,
    #[prost(bool, tag = "3")]
    pub outcome_unknown: bool,
    #[prost(string, optional, tag = "4")]
    pub error_code: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub message: Option<String>,
    #[prost(oneof = "invocation_terminal_v1::Result", tags = "10, 11, 12, 13, 14")]
    pub result: Option<invocation_terminal_v1::Result>,
}
#[derive(Clone, PartialEq, Message)]
pub struct LlmResultV1 {
    #[prost(string, optional, tag = "1")]
    pub provider_session_id: Option<String>,
    #[prost(message, repeated, tag = "2")]
    pub output: Vec<TypedBinaryChunkV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpeechToTextResultV1 {
    #[prost(string, tag = "1")]
    pub transcript: String,
    #[prost(string, optional, tag = "2")]
    pub language: Option<String>,
    #[prost(float, optional, tag = "3")]
    pub confidence: Option<f32>,
    #[prost(message, repeated, tag = "4")]
    pub segments: Vec<TypedBinaryChunkV1>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TextToSpeechResultV1 {
    #[prost(bytes = "vec", tag = "1")]
    pub audio: Vec<u8>,
    #[prost(string, tag = "2")]
    pub media_type: String,
    #[prost(uint32, optional, tag = "3")]
    pub sample_rate: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub metadata_json: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PersistenceResultV1 {
    #[prost(string, tag = "1")]
    pub operation_name: String,
    #[prost(message, repeated, tag = "2")]
    pub records: Vec<TypedBinaryChunkV1>,
}

pub mod invocation_terminal_v1 {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum Result {
        #[prost(message, tag = "10")]
        Llm(LlmResultV1),
        #[prost(message, tag = "11")]
        SpeechToText(SpeechToTextResultV1),
        #[prost(message, tag = "12")]
        TextToSpeech(TextToSpeechResultV1),
        #[prost(message, tag = "13")]
        Semantic(PersistenceResultV1),
        #[prost(message, tag = "14")]
        Relational(PersistenceResultV1),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct ReadinessRequestV1 {
    #[prost(uint32, repeated, tag = "1")]
    pub supported_majors: Vec<u32>,
    #[prost(uint32, repeated, tag = "2")]
    pub supported_minors: Vec<u32>,
    #[prost(uint64, tag = "3")]
    pub deadline_unix_ms: u64,
    #[prost(string, tag = "4")]
    pub client_instance_id: String,
    #[prost(enumeration = "ResourceCapabilityV1", repeated, tag = "5")]
    pub requested_capabilities: Vec<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Enumeration)]
#[repr(i32)]
pub enum ResourceCapabilityV1 {
    LlmExecution = 0,
    SpeechInference = 1,
    GraphPersistence = 2,
    SqlPersistence = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Enumeration)]
#[repr(i32)]
pub enum CapabilityReadinessV1 {
    Ready = 0,
    NotConfigured = 1,
    Recovering = 2,
    Unavailable = 3,
}

#[derive(Clone, PartialEq, Message)]
pub struct CapabilityReadinessEntryV1 {
    #[prost(enumeration = "ResourceCapabilityV1", tag = "1")]
    pub capability: i32,
    #[prost(enumeration = "CapabilityReadinessV1", tag = "2")]
    pub status: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct LlmProviderDescriptorV1 {
    #[prost(string, tag = "1")]
    pub provider_id: String,
    #[prost(uint32, tag = "2")]
    pub concurrency: u32,
    #[prost(bytes = "vec", tag = "3")]
    pub capabilities: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct SpeechDescriptorV1 {
    #[prost(string, tag = "1")]
    pub adapter_id: String,
    #[prost(string, repeated, tag = "2")]
    pub models: Vec<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ReadinessResponseV1 {
    #[prost(string, tag = "1")]
    pub tenant_id: String,
    #[prost(string, tag = "2")]
    pub server_instance_id: String,
    #[prost(uint32, tag = "3")]
    pub negotiated_minor: u32,
    #[prost(message, repeated, tag = "4")]
    pub capabilities: Vec<CapabilityReadinessEntryV1>,
    #[prost(message, repeated, tag = "5")]
    pub llm_providers: Vec<LlmProviderDescriptorV1>,
    #[prost(message, repeated, tag = "6")]
    pub speech: Vec<SpeechDescriptorV1>,
}
