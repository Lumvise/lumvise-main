//! App-facing compiled-plugin invocation contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lumvise_plugin_runtime::{
    PluginInvocationClass as RuntimeInvocationClass,
    PluginInvocationContext as RuntimeInvocationContext,
    PluginInvocationRequest as RuntimeInvocationRequest,
};

static NEXT_SURFACE_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// One request routed to a ready compiled MCP export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInvocationRequest {
    pub tool_name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct PluginInvocationLifecycle {
    pub(crate) request_id: String,
    pub(crate) owner_id: String,
    pub(crate) session_id: String,
    pub(crate) scope_id: Option<String>,
    pub(crate) deadline: Instant,
}

pub(crate) fn local_invocation_lifecycle(
    owner_id: &str,
    scope_id: Option<&str>,
) -> PluginInvocationLifecycle {
    let sequence = NEXT_SURFACE_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    PluginInvocationLifecycle {
        request_id: format!("local-{}-{sequence}", std::process::id()),
        owner_id: owner_id.to_owned(),
        session_id: format!("local-session-{}", std::process::id()),
        scope_id: scope_id.map(str::to_owned),
        deadline: Instant::now() + Duration::from_secs(60),
    }
}

pub(crate) fn surface_invocation_request(
    plugin_id: &str,
    export_id: &str,
    input: Value,
    owner_id: &str,
    deadline: Instant,
) -> RuntimeInvocationRequest {
    let context = surface_invocation_context(owner_id, deadline);
    RuntimeInvocationRequest::new(plugin_id, export_id, input, context)
}

pub(crate) fn surface_invocation_context(
    owner_id: &str,
    deadline: Instant,
) -> RuntimeInvocationContext {
    let sequence = NEXT_SURFACE_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    RuntimeInvocationContext::new(
        format!("surface-{}-{sequence}", std::process::id()),
        owner_id,
        RuntimeInvocationClass::Foreground,
        deadline,
    )
}

pub(crate) fn background_invocation_request(
    plugin_id: &str,
    export_id: &str,
    input: Value,
    delivery_id: &str,
    deadline: Instant,
) -> RuntimeInvocationRequest {
    let context = RuntimeInvocationContext::new(
        format!("background:{delivery_id}"),
        "plugin-background-driver",
        RuntimeInvocationClass::Background,
        deadline,
    );
    RuntimeInvocationRequest::new(plugin_id, export_id, input, context)
}

/// One normalized compiled-plugin invocation outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInvocationResponse {
    pub status: PluginInvocationStatus,
    pub output: Value,
    pub job_id: Option<String>,
}

/// Host-visible state of one compiled invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginInvocationStatus {
    Completed,
    Accepted,
    Failed,
}
