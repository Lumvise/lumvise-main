//! Generic MCP JSON-RPC, stdio, and app-bridge transports.

pub mod app_bridge_transport;
mod application;
mod bridge_protocol;
mod error;
mod invocation;
mod server;
mod stdio;

pub use application::{McpApplication, McpApplicationError, McpInvocationFailureKind, McpTool};
pub use bridge_protocol::{
    APP_BRIDGE_PROTOCOL_MAJOR, APP_RUNTIME_CONTROL_PROTOCOL_MAJOR, AppBridgeCancellationRequestV1,
    AppBridgeCancellationResponseV1, AppBridgeInvocationRequestV1, AppBridgeInvocationResponseV1,
    AppBridgeInvocationStatusV1, RuntimeControlKindV1, RuntimeControlRequestV1,
    RuntimeControlResponseV1, RuntimeControlStateV1, decode_runtime_control_frame,
    frame_runtime_control,
};
pub use error::{McpCoreError, Result};
pub use invocation::{McpInvocationCancellation, McpInvocationContext};
pub use server::LumviseMcpServer;
pub use stdio::run_stdio;
