use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpCapability {
    Governance,
    SemanticIndexing,
    NativeExecution,
    AssistantBridge,
    WhiteboardBridge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisterMcpRequest {
    pub instance_id: String,
    pub project_root: String,
    pub display_name: Option<String>,
    pub capabilities: Vec<McpCapability>,
    pub control_channel: Option<ControlChannelDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlChannelDescriptor {
    WebSocket { url: String },
    HttpCallback { base_url: String },
    AppManagedStdio { launch_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisterMcpResponse {
    pub accepted: bool,
    pub instance: RegisteredMcpInstance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatRequest {
    pub instance_id: String,
    pub project_root: String,
    pub status: McpInstanceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpInstanceStatus {
    Starting,
    Ready,
    Busy,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredMcpInstance {
    pub instance_id: String,
    pub project_root: String,
    pub display_name: String,
    pub capabilities: Vec<McpCapability>,
    pub status: McpInstanceStatus,
    pub registered_at: String,
    pub last_seen_at: String,
    pub control_channel: Option<ControlChannelDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpInstanceListResponse {
    pub instances: Vec<RegisteredMcpInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleMcpCleanupRequest {
    pub stale_after_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleMcpCleanupResponse {
    pub marked_unavailable: usize,
    pub stale_before: String,
}
