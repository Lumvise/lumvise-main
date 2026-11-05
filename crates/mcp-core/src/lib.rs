//! Generic MCP JSON-RPC, stdio, and app-bridge transports.

pub mod app_bridge_transport;
mod application;
mod bridge_protocol;
mod error;
mod invocation;
mod server;
mod stdio;
mod tool_content;

pub use application::{McpApplication, McpApplicationError, McpInvocationFailureKind, McpTool};
pub use bridge_protocol::{
    APP_BRIDGE_PROTOCOL_MAJOR, APP_RUNTIME_CONTROL_PROTOCOL_MAJOR, AppBridgeCancellationRequestV3,
    AppBridgeCancellationResponseV3, AppBridgeInvocationRequestV3, AppBridgeInvocationResponseV3,
    AppBridgeInvocationStatusV3, RuntimeControlKindV1, RuntimeControlRequestV1,
    RuntimeControlResponseV1, RuntimeControlStateV1, decode_runtime_control_frame,
    frame_runtime_control,
};
pub use error::{McpCoreError, Result};
pub use invocation::{McpInvocationCancellation, McpInvocationContext};
pub use server::LumviseMcpServer;
pub use stdio::run_stdio;
pub use tool_content::encode_mcp_tool_result;
