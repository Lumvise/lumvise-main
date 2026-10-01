//! Plugin-to-plugin invocation policy and cycle protection.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use lumvise_plugin_package::ExportSurface;
use lumvise_plugin_protocol::WireOutcome;
use serde_json::Value;

use super::PluginSystem;
use crate::{HostCapabilityError, PluginInvocationContext, PluginRuntimeError, SchemaDirection};

impl PluginSystem {
    pub(crate) async fn invoke_plugin_async(
        &self,
        caller_plugin_id: &str,
        input: Value,
        parent_context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        let request = PluginInvokeRequest::parse(input)?;
        if caller_plugin_id == request.plugin_id {
            return Err(plugin_invoke_error(
                "plugin_invoke_self_denied",
                format!("plugin `{caller_plugin_id}` cannot invoke itself"),
                false,
            ));
        }
        let _wait = InvocationWaitGuard::enter(self, caller_plugin_id, &request.plugin_id)?;
        self.require_public_mcp_export(&request.plugin_id, &request.export_id)?;
        let nested_context = nested_context(parent_context)?;
        let outcome = self
            .invoke_ready_export_async(
                &request.plugin_id,
                &request.export_id,
                request.input,
                &nested_context,
            )
            .await
            .map_err(map_plugin_invoke_failure)?;
        map_target_outcome(outcome)
    }
}

fn nested_context(
    parent: &PluginInvocationContext,
) -> Result<PluginInvocationContext, HostCapabilityError> {
    const MAX_RESPONSE_GRACE: Duration = Duration::from_millis(50);
    let remaining = parent.deadline().saturating_duration_since(Instant::now());
    let response_grace = MAX_RESPONSE_GRACE.min(remaining / 4);
    let Some(deadline) = parent.deadline().checked_sub(response_grace) else {
        return Err(parent_deadline_exhausted());
    };
    if deadline <= Instant::now() {
        return Err(parent_deadline_exhausted());
    }
    Ok(parent.clone().with_deadline_cap(deadline))
}

fn parent_deadline_exhausted() -> HostCapabilityError {
    plugin_invoke_error(
        "plugin_invoke_target_timeout",
        "parent invocation deadline is exhausted".to_owned(),
        true,
    )
}

fn map_target_outcome(outcome: WireOutcome) -> Result<Value, HostCapabilityError> {
    match outcome {
        WireOutcome::Succeeded { value } if value.is_object() => {
            Ok(serde_json::json!({"output": value}))
        }
        WireOutcome::Succeeded { value } => Err(plugin_invoke_error(
            "plugin_invoke_target_output_invalid",
            format!("target output `{value}`; expected object"),
            false,
        )),
        WireOutcome::Failed { error } => {
            let message = format!(
                "target export failed with `{}`: {}",
                error.code, error.message
            );
            Err(
                plugin_invoke_error("plugin_invoke_target_failed", message, error.retryable)
                    .with_details(serde_json::json!({"target_error": {
                        "code": error.code, "message": error.message, "details": error.details
                    }})),
            )
        }
    }
}

impl PluginSystem {
    fn require_public_mcp_export(
        &self,
        plugin_id: &str,
        export_id: &str,
    ) -> Result<(), HostCapabilityError> {
        let entry = self.entry(plugin_id).map_err(map_plugin_invoke_failure)?;
        if !entry.is_ready() {
            return Err(plugin_invoke_error(
                "plugin_invoke_target_not_ready",
                format!("target plugin `{plugin_id}` is not ready"),
                true,
            ));
        }
        let export = entry
            .exports()
            .iter()
            .find(|export| export.id == export_id)
            .ok_or_else(|| missing_export(plugin_id, export_id))?;
        if export.surface != ExportSurface::McpTool {
            return Err(plugin_invoke_error(
                "plugin_invoke_export_surface_denied",
                format!("target export `{plugin_id}:{export_id}` is not a global MCP tool"),
                false,
            ));
        }
        Ok(())
    }
}

fn missing_export(plugin_id: &str, export_id: &str) -> HostCapabilityError {
    plugin_invoke_error(
        "plugin_invoke_export_absent",
        format!("target plugin `{plugin_id}` has no export `{export_id}`"),
        false,
    )
}

struct PluginInvokeRequest {
    plugin_id: String,
    export_id: String,
    input: Value,
}

impl PluginInvokeRequest {
    fn parse(input: Value) -> Result<Self, HostCapabilityError> {
        let Some(fields) = input.as_object() else {
            return Err(invalid_plugin_invoke_input(&input));
        };
        if fields.len() != 3 || !fields.contains_key("input") {
            return Err(invalid_plugin_invoke_input(&input));
        }
        let plugin_id = invoke_string(fields, "plugin_id", &input)?;
        let export_id = invoke_string(fields, "export_id", &input)?;
        let Some(nested_input) = fields.get("input").filter(|value| value.is_object()) else {
            return Err(invalid_plugin_invoke_input(&input));
        };
        Ok(Self {
            plugin_id: plugin_id.to_owned(),
            export_id: export_id.to_owned(),
            input: nested_input.clone(),
        })
    }
}

fn invoke_string<'value>(
    fields: &'value serde_json::Map<String, Value>,
    name: &str,
    input: &Value,
) -> Result<&'value str, HostCapabilityError> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_plugin_invoke_input(input))
}

fn invalid_plugin_invoke_input(input: &Value) -> HostCapabilityError {
    plugin_invoke_error(
        "plugin_invoke_invalid_input",
        format!(
            "input `{input}`; expected exactly {{plugin_id:string,export_id:string,input:object}}"
        ),
        false,
    )
}

fn map_plugin_invoke_failure(error: PluginRuntimeError) -> HostCapabilityError {
    let (code, retryable) = match &error {
        PluginRuntimeError::NotInstalled(_) => ("plugin_invoke_target_absent", false),
        PluginRuntimeError::NotReady(_) => ("plugin_invoke_target_not_ready", true),
        PluginRuntimeError::UndeclaredExport { .. } => ("plugin_invoke_export_absent", false),
        PluginRuntimeError::SchemaValidation {
            direction: SchemaDirection::Input,
            ..
        } => ("plugin_invoke_target_input_invalid", false),
        PluginRuntimeError::SchemaValidation {
            direction: SchemaDirection::Output,
            ..
        } => ("plugin_invoke_target_output_invalid", false),
        PluginRuntimeError::InvocationCycle { .. } => ("plugin_invoke_cycle_denied", false),
        PluginRuntimeError::InvocationTimeout { .. } => ("plugin_invoke_target_timeout", true),
        PluginRuntimeError::ProcessExited { .. } => ("plugin_invoke_target_crashed", true),
        _ => ("plugin_invoke_target_transport_failed", true),
    };
    plugin_invoke_error(code, error.to_string(), retryable)
}

fn plugin_invoke_error(code: &str, message: String, retryable: bool) -> HostCapabilityError {
    HostCapabilityError::new("plugin.invoke", code, message, retryable)
}

struct InvocationWaitGuard<'system> {
    system: &'system PluginSystem,
    caller_plugin_id: String,
}

impl<'system> InvocationWaitGuard<'system> {
    fn enter(
        system: &'system PluginSystem,
        caller_plugin_id: &str,
        target_plugin_id: &str,
    ) -> Result<Self, HostCapabilityError> {
        let mut waits = system.invocation_waits.lock().map_err(|_| {
            plugin_invoke_error(
                "plugin_invoke_wait_graph_unavailable",
                "plugin invocation wait graph lock is poisoned".to_owned(),
                true,
            )
        })?;
        if closes_wait_cycle(&waits, caller_plugin_id, target_plugin_id) {
            return Err(plugin_invoke_error(
                "plugin_invoke_cycle_denied",
                format!(
                    "invocation edge `{caller_plugin_id}` -> `{target_plugin_id}` closes a cycle"
                ),
                false,
            ));
        }
        waits.insert(caller_plugin_id.to_owned(), target_plugin_id.to_owned());
        Ok(Self {
            system,
            caller_plugin_id: caller_plugin_id.to_owned(),
        })
    }
}

impl Drop for InvocationWaitGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut waits) = self.system.invocation_waits.lock() {
            waits.remove(&self.caller_plugin_id);
        }
    }
}

fn closes_wait_cycle(
    waits: &HashMap<String, String>,
    caller_plugin_id: &str,
    target_plugin_id: &str,
) -> bool {
    let mut current = target_plugin_id;
    let mut visited = std::collections::HashSet::new();
    while visited.insert(current) {
        if current == caller_plugin_id {
            return true;
        }
        let Some(next) = waits.get(current) else {
            return false;
        };
        current = next;
    }
    true
}
