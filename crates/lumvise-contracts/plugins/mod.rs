use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginMcpSurface {
    pub plugins: Vec<PluginMcpPlugin>,
    pub capabilities: Vec<PluginMcpCapability>,
    pub assistant_routines: Vec<PluginMcpRoutine>,
    pub event_routines: Vec<PluginMcpRoutine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PluginMcpSurfaceAccess>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMcpSurfaceAccess {
    pub status: String,
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contention: Option<PluginMcpSurfaceContention>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMcpSurfaceContention {
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMcpPlugin {
    pub plugin_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMcpCapability {
    pub plugin_id: String,
    pub capability_id: String,
    pub write_impact: String,
    pub approval_required: bool,
    pub autonomous_possible: bool,
    pub dynamic_tool_name: String,
    pub invocation_status: String,
    pub unavailable_reason: Option<String>,
    pub response_mode: String,
    pub requires_existing_procedure: bool,
    pub route_aliases: Vec<PluginMcpRouteAlias>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMcpRouteAlias {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginMcpRoutine {
    pub plugin_id: String,
    pub routine_id: String,
    pub stages: Vec<String>,
    pub write_impact: String,
    pub approval_required: bool,
    pub autonomous_possible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginInvocationEntrypoint {
    Mcp,
    AssistantRoutine,
    EventRoutine,
    ScheduledRoutine,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginInvocationActor {
    pub actor_type: String,
    pub actor_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginCapabilityInvocationRequest {
    pub plugin_id: String,
    pub capability_id: String,
    pub entrypoint: PluginInvocationEntrypoint,
    pub actor: PluginInvocationActor,
    pub target_semantic_element_id: Option<String>,
    pub input: Value,
    pub autonomous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginOperationInvocationRequest {
    pub plugin_id: String,
    pub operation_id: String,
    pub actor: PluginInvocationActor,
    pub target_semantic_element_id: Option<String>,
    pub input: Value,
    pub autonomous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginCapabilityInvocationResponse {
    pub plugin_id: String,
    pub capability_id: String,
    pub status: String,
    pub output: Value,
}

pub type PluginOperationInvocationResponse = PluginCapabilityInvocationResponse;
