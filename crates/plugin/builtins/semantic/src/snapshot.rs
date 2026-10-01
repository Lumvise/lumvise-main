use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

pub(crate) fn create(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    invoke(input, "create", context)
}

pub(crate) fn status(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    invoke(input, "status", context)
}

pub(crate) fn cancel(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    invoke(input, "cancel", context)
}

fn invoke(
    mut input: Value,
    operation: &str,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let object = input.as_object_mut().ok_or_else(|| {
        PluginError::new(
            "invalid_semantic_input",
            "semantic snapshot input must be an object",
            false,
        )
    })?;
    object.insert("operation".into(), json!(operation));
    context.host_call("semantic.snapshot", input)
}
