//! Existing semantic snapshot capability; extraction keeps the routing module focused.
use super::{
    AppCoreHostCapabilityBroker, SEMANTIC_SNAPSHOT_CAPABILITY, invalid_input, optional_string,
    require_active_host_call,
};
use lumvise_plugin_runtime::{HostCapabilityError, PluginInvocationContext};
use serde_json::Value;

impl AppCoreHostCapabilityBroker {
    pub(super) fn invoke_semantic_snapshot(
        &self,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        let operation = input
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("create");
        let snapshot_operation = match operation {
            "create" => self.create_semantic_snapshot_for_input(&input)?,
            "status" => {
                let operation_id = required_snapshot_operation_id(&input)?;
                self.semantic_snapshots
                    .status(operation_id)
                    .map_err(|error| snapshot_error(error.to_string()))?
            }
            "cancel" => {
                let operation_id = required_snapshot_operation_id(&input)?;
                self.semantic_snapshots
                    .cancel(operation_id)
                    .map_err(|error| snapshot_error(error.to_string()))?
            }
            other => {
                return Err(snapshot_error(format!(
                    "unknown semantic snapshot operation `{other}`"
                )));
            }
        };
        serde_json::to_value(snapshot_operation).map_err(|error| snapshot_error(error.to_string()))
    }

    fn create_semantic_snapshot_for_input(
        &self,
        input: &Value,
    ) -> Result<crate::SemanticSnapshotOperation, HostCapabilityError> {
        let fields = input
            .as_object()
            .ok_or_else(|| invalid_input(input, "semantic snapshot object"))?;
        let operation = match optional_string(fields, "project_root")? {
            Some(root) => self.semantic_snapshots.create_for_project(root),
            None => self.semantic_snapshots.create(),
        };
        operation.map_err(|error| snapshot_error(error.to_string()))
    }

    pub(super) fn invoke_semantic_snapshot_controlled(
        &self,
        input: Value,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        require_active_host_call(SEMANTIC_SNAPSHOT_CAPABILITY, context)?;
        self.invoke_semantic_snapshot(input)
    }
}

fn snapshot_error(message: impl Into<String>) -> HostCapabilityError {
    HostCapabilityError::new(
        SEMANTIC_SNAPSHOT_CAPABILITY,
        "host_capability_execution_failed",
        message,
        false,
    )
}

fn required_snapshot_operation_id(input: &Value) -> Result<&str, HostCapabilityError> {
    input
        .get("operation_id")
        .and_then(Value::as_str)
        .ok_or_else(|| snapshot_error("operation_id is required"))
}
