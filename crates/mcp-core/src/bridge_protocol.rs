use prost::{Enumeration, Message};

/// Current major for the MCP-to-app Protobuf invocation envelope.
pub const APP_BRIDGE_PROTOCOL_MAJOR: u32 = 1;

/// Versioned MCP invocation sent from the standalone adapter to App Core.
#[derive(Clone, PartialEq, Message)]
pub struct AppBridgeInvocationRequestV1 {
    /// Wire protocol major.
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    /// JSON-RPC request correlation identity.
    #[prost(string, tag = "2")]
    pub request_id: String,
    /// Stable MCP owner identity.
    #[prost(string, tag = "3")]
    pub owner_id: String,
    /// MCP transport session identity.
    #[prost(string, tag = "4")]
    pub session_id: String,
    /// Optional signed scoped MCP identity.
    #[prost(string, optional, tag = "5")]
    pub scope_id: Option<String>,
    /// Absolute Unix-epoch deadline in milliseconds.
    #[prost(uint64, tag = "6")]
    pub deadline_unix_ms: u64,
    /// Target Plugin identity.
    #[prost(string, tag = "7")]
    pub plugin_id: String,
    /// Target signed export identity.
    #[prost(string, tag = "8")]
    pub capability_id: String,
    /// UTF-8 JSON input retained as opaque bytes inside the binary envelope.
    #[prost(bytes = "vec", tag = "9")]
    pub input_json: Vec<u8>,
}

/// Stable typed outcome returned by the app bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enumeration)]
#[repr(i32)]
pub enum AppBridgeInvocationStatusV1 {
    /// Plugin completed successfully.
    Completed = 0,
    /// Plugin-local admission is full.
    Busy = 1,
    /// Ingress deadline elapsed.
    DeadlineExceeded = 2,
    /// Caller cancelled the request.
    Cancelled = 3,
    /// Target is temporarily unavailable.
    Unavailable = 4,
    /// Plugin returned or caused a terminal failure.
    Failed = 5,
    /// App or Runtime internal state failed.
    Internal = 6,
}

/// Versioned typed result returned from App Core.
#[derive(Clone, PartialEq, Message)]
pub struct AppBridgeInvocationResponseV1 {
    /// Wire protocol major.
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    /// Correlated request identity.
    #[prost(string, tag = "2")]
    pub request_id: String,
    /// Stable terminal category.
    #[prost(enumeration = "AppBridgeInvocationStatusV1", tag = "3")]
    pub status: i32,
    /// Successful or Plugin-returned JSON payload.
    #[prost(bytes = "vec", tag = "4")]
    pub output_json: Vec<u8>,
    /// Host diagnostic for non-success outcomes.
    #[prost(string, tag = "5")]
    pub message: String,
    /// Whether retrying unchanged can succeed later.
    #[prost(bool, tag = "6")]
    pub retryable: bool,
}

/// Versioned cancellation request for one active bridge invocation.
#[derive(Clone, PartialEq, Message)]
pub struct AppBridgeCancellationRequestV1 {
    /// Wire protocol major.
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    /// Correlated request identity.
    #[prost(string, tag = "2")]
    pub request_id: String,
    /// Owning MCP identity required for cancellation authorization.
    #[prost(string, tag = "3")]
    pub owner_id: String,
    /// Owning MCP session identity.
    #[prost(string, tag = "4")]
    pub session_id: String,
    /// Optional scoped route identity.
    #[prost(string, optional, tag = "5")]
    pub scope_id: Option<String>,
    /// Target Plugin identity bound to the active invocation.
    #[prost(string, tag = "6")]
    pub plugin_id: String,
}

/// Versioned acknowledgement for a bridge cancellation request.
#[derive(Clone, PartialEq, Message)]
pub struct AppBridgeCancellationResponseV1 {
    /// Wire protocol major.
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    /// Whether a matching active invocation was cancelled.
    #[prost(bool, tag = "2")]
    pub cancelled: bool,
}
/// Current major for the authenticated App Runtime control protocol.
pub const APP_RUNTIME_CONTROL_PROTOCOL_MAJOR: u32 = 1;

/// Runtime-control command kind. Activation arguments are typed Protobuf fields;
/// no command embeds a secondary JSON wire payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Enumeration)]
#[repr(i32)]
pub enum RuntimeControlKindV1 {
    Handshake = 0,
    Activate = 1,
    Quit = 2,
    State = 3,
}

/// Authenticated, generation-bound request on the local Runtime control seam.
#[derive(Clone, PartialEq, Message)]
pub struct RuntimeControlRequestV1 {
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    #[prost(string, tag = "2")]
    pub request_id: String,
    /// Runtime generation metadata from discovery; never a bearer credential.
    #[prost(string, tag = "3")]
    pub generation_nonce: String,
    /// Private credential held beside the ownership lease.
    #[prost(string, tag = "4")]
    pub owner_credential: String,
    #[prost(enumeration = "RuntimeControlKindV1", tag = "5")]
    pub kind: i32,
    #[prost(string, repeated, tag = "6")]
    pub activation_arguments: Vec<String>,
    #[prost(uint64, tag = "7")]
    pub deadline_unix_ms: u64,
}

/// Typed Runtime control response. Endpoint and credential are returned only
/// after a successful handshake.
#[derive(Clone, PartialEq, Message)]
pub struct RuntimeControlResponseV1 {
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    #[prost(string, tag = "2")]
    pub request_id: String,
    #[prost(string, tag = "3")]
    pub generation_nonce: String,
    #[prost(enumeration = "RuntimeControlStateV1", tag = "4")]
    pub state: i32,
    /// Whether the command was accepted by Runtime Control.
    #[prost(bool, tag = "5")]
    pub accepted: bool,
    #[prost(string, tag = "6")]
    pub error_code: String,
    #[prost(string, tag = "7")]
    pub message: String,
    #[prost(string, tag = "8")]
    pub app_bridge_base_url: String,
    /// Issued only by a successful handshake.
    #[prost(string, tag = "9")]
    pub app_bridge_credential: String,
    #[prost(uint64, tag = "10")]
    pub retry_after_ms: u64,
    /// Activation's foreground result is independent of command acceptance.
    #[prost(bool, tag = "11")]
    pub foreground_succeeded: bool,
    /// Absolute expiry for the issued App Bridge credential.
    #[prost(uint64, tag = "12")]
    pub credential_expires_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enumeration)]
#[repr(i32)]
pub enum RuntimeControlStateV1 {
    Absent = 0,
    Starting = 1,
    Ready = 2,
    Quitting = 3,
    Exited = 4,
}

/// Prefixes one Protobuf message with its four-byte network-order frame size.
pub fn frame_runtime_control<M: Message>(message: &M) -> Vec<u8> {
    let payload = message.encode_to_vec();
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(&payload);
    framed
}

/// Decodes one dynamically sized Runtime control frame.
pub fn decode_runtime_control_frame<M: Message + Default>(frame: &[u8]) -> Result<M, &'static str> {
    if frame.len() < 4 {
        return Err("runtime control frame missing length prefix");
    }
    let size = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if frame.len() != size.saturating_add(4) {
        return Err("runtime control frame length mismatch");
    }
    M::decode(&frame[4..]).map_err(|_| "runtime control protobuf decode failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_round_trip_preserves_absolute_deadline_and_owner_tuple() {
        let request = AppBridgeInvocationRequestV1 {
            protocol_major: APP_BRIDGE_PROTOCOL_MAJOR,
            request_id: "request-1".into(),
            owner_id: "owner-1".into(),
            session_id: "session-1".into(),
            scope_id: Some("scope-1".into()),
            deadline_unix_ms: 9_999,
            plugin_id: "plugin-1".into(),
            capability_id: "run".into(),
            input_json: br#"{"value":1}"#.to_vec(),
        };

        let decoded = AppBridgeInvocationRequestV1::decode(request.encode_to_vec().as_slice())
            .expect("decode invocation");
        assert_eq!(decoded, request);

        let request = RuntimeControlRequestV1 {
            protocol_major: APP_RUNTIME_CONTROL_PROTOCOL_MAJOR,
            request_id: "request-2".into(),
            generation_nonce: "generation-1".into(),
            owner_credential: "credential-1".into(),
            kind: RuntimeControlKindV1::Activate as i32,
            activation_arguments: vec!["--file".into(), "/tmp/note.md".into()],
            deadline_unix_ms: 10_000,
        };

        let frame = frame_runtime_control(&request);
        let decoded: RuntimeControlRequestV1 =
            decode_runtime_control_frame(&frame).expect("decode runtime control");

        assert_eq!(decoded, request);
    }
}
