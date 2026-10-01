use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{McpCapability, McpInstanceStatus};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlConnectionRecord {
    pub instance_id: String,
    pub project_root: String,
    pub status: McpInstanceStatus,
    pub native_control_ready: bool,
    pub connected_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlConnectionListResponse {
    pub connections: Vec<ControlConnectionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlClientMessage {
    Hello {
        instance_id: String,
        project_root: String,
        capabilities: Vec<McpCapability>,
    },
    Heartbeat {
        instance_id: String,
        project_root: String,
        status: McpInstanceStatus,
    },
    CommandResult {
        instance_id: String,
        command_id: String,
        ok: bool,
        summary: String,
        #[serde(default)]
        output: Option<Value>,
    },
    CommandProgress {
        instance_id: String,
        command_id: String,
        message: String,
        #[serde(default)]
        output: Option<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlServerMessage {
    Accepted {
        instance_id: String,
        policy: ExecutionBoundaryPolicy,
    },
    Ping {
        nonce: String,
    },
    ExecuteCommand {
        command_id: String,
        operation: String,
        payload: Value,
        policy: ExecutionBoundaryPolicy,
    },
    CancelCommand {
        command_id: String,
        reason: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionBoundaryPolicy {
    pub mode: ExecutionBoundaryMode,
    pub allowed_operations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBoundaryMode {
    AllowAllForDevelopment,
    AllowListed,
}

impl ExecutionBoundaryPolicy {
    pub fn allow_all_for_development() -> Self {
        Self {
            mode: ExecutionBoundaryMode::AllowAllForDevelopment,
            allowed_operations: Vec::new(),
        }
    }

    pub fn allow_listed(allowed_operations: Vec<String>) -> Self {
        Self {
            mode: ExecutionBoundaryMode::AllowListed,
            allowed_operations,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandDispatchRequest {
    pub instance_id: String,
    pub operation: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub policy: Option<ExecutionBoundaryPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandDispatchResponse {
    pub command_id: String,
    pub instance_id: String,
    pub dispatched: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandCancelRequest {
    pub instance_id: String,
    pub command_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandCancelResponse {
    pub command_id: String,
    pub instance_id: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandProgressRecord {
    pub instance_id: String,
    pub command_id: String,
    pub message: String,
    pub output: Option<Value>,
    pub received_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandProgressListResponse {
    pub progress: Vec<ControlCommandProgressRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandResultRecord {
    pub instance_id: String,
    pub command_id: String,
    pub ok: bool,
    pub summary: String,
    pub output: Option<Value>,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCommandResultListResponse {
    pub results: Vec<ControlCommandResultRecord>,
}
