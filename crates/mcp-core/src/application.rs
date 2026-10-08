use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::McpInvocationContext;

/// Application seam consumed by MCP transports.
///
/// Implementations own tool discovery, argument validation, routing, and
/// domain behavior. MCP Core only translates this interface to JSON-RPC.
pub trait McpApplication: Send + Sync {
    /// Observes a completed MCP handshake; e.g. begin connection-scoped registration.
    fn mcp_client_initialized(&self) -> Result<(), McpApplicationError> {
        Ok(())
    }

    /// Retires this transport before invocation draining; e.g. stop presence renewal.
    fn mcp_client_disconnected(&self) {}

    /// Returns complete tool descriptors visible to MCP clients.
    fn list_tools(&self) -> Result<Vec<McpTool>, McpApplicationError>;

    /// Invokes one tool using its externally advertised name.
    fn invoke_tool(&self, name: &str, arguments: Value) -> Result<Value, McpApplicationError>;

    /// Invokes one tool while preserving its ingress lifecycle context.
    fn invoke_tool_controlled(
        &self,
        _context: &McpInvocationContext,
        name: &str,
        arguments: Value,
    ) -> Result<Value, McpApplicationError> {
        self.invoke_tool(name, arguments)
    }

    /// Propagates a JSON-RPC cancellation notification downstream.
    fn cancel_invocation(&self, context: &McpInvocationContext) -> Result<(), McpApplicationError> {
        context.cancellation().cancel();
        Ok(())
    }
}

/// Stable MCP-visible terminal category for accepted tool invocations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpInvocationFailureKind {
    /// Target-local bounded admission is full.
    Busy,
    /// The ingress absolute deadline elapsed.
    DeadlineExceeded,
    /// The caller cancelled the request.
    Cancelled,
    /// The target is temporarily unavailable.
    Unavailable,
    /// Execution failed after admission.
    Failed,
}

/// Transport-neutral MCP tool descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl McpTool {
    /// Creates one descriptor from its public name, description, and JSON Schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }

    /// Decodes an externally discovered descriptor using MCP field names.
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}

/// Failure returned by an application adapter to MCP transport.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum McpApplicationError {
    /// Caller supplied an unknown tool or invalid arguments.
    #[error("{0}")]
    InvalidParams(String),

    /// Tool execution failed after a valid invocation was accepted.
    #[error("{0}")]
    Invocation(String),

    /// Typed failure retained across the MCP application boundary.
    #[error("{message}")]
    ControlledInvocation {
        /// Stable terminal category.
        kind: McpInvocationFailureKind,
        /// Caller-facing diagnostic.
        message: String,
        /// Whether retrying later can succeed unchanged.
        retryable: bool,
    },
}

impl McpApplicationError {
    /// Creates an invalid-parameters failure with application-owned detail.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::InvalidParams(message.into())
    }

    /// Creates an invocation failure with application-owned detail.
    pub fn invocation(message: impl Into<String>) -> Self {
        Self::Invocation(message.into())
    }

    /// Creates one typed invocation failure.
    pub fn controlled_invocation(
        kind: McpInvocationFailureKind,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self::ControlledInvocation {
            kind,
            message: message.into(),
            retryable,
        }
    }
}
