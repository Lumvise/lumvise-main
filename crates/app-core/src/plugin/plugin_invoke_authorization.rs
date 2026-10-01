//! Validates the authorization half of the generic `plugin.invoke` Host Capability.
//!
//! Plugin Runtime owns target lookup and execution. App Core only validates the
//! policy-authorized request shape, preventing a second invocation implementation.

use lumvise_plugin_runtime::HostCapabilityError;
use serde_json::{Map, Value, json};

use super::host_capability_catalog::PLUGIN_INVOKE_CAPABILITY;

pub(crate) fn authorize_plugin_invoke(input: Value) -> Result<Value, HostCapabilityError> {
    let fields = input
        .as_object()
        .ok_or_else(|| invalid_plugin_invoke_input(&input))?;
    require_exact_fields(fields, &input)?;
    require_nonempty_string(fields, "plugin_id", &input)?;
    require_nonempty_string(fields, "export_id", &input)?;
    if !fields.get("input").is_some_and(Value::is_object) {
        return Err(invalid_plugin_invoke_input(&input));
    }
    Ok(json!({}))
}

fn require_exact_fields(
    fields: &Map<String, Value>,
    input: &Value,
) -> Result<(), HostCapabilityError> {
    if fields.len() == 3
        && fields.contains_key("plugin_id")
        && fields.contains_key("export_id")
        && fields.contains_key("input")
    {
        return Ok(());
    }
    Err(invalid_plugin_invoke_input(input))
}

fn require_nonempty_string(
    fields: &Map<String, Value>,
    name: &str,
    input: &Value,
) -> Result<(), HostCapabilityError> {
    if fields
        .get(name)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(());
    }
    Err(invalid_plugin_invoke_input(input))
}

fn invalid_plugin_invoke_input(input: &Value) -> HostCapabilityError {
    HostCapabilityError::new(
        PLUGIN_INVOKE_CAPABILITY,
        "invalid_host_capability_input",
        format!(
            "input `{input}`; expected exactly {{plugin_id:string,export_id:string,input:object}}"
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_accepts_exact_plugin_invoke_contract() {
        let result = authorize_plugin_invoke(json!({
            "plugin_id": "semantic",
            "export_id": "semantic.search",
            "input": {"query": "module"}
        }));

        assert_eq!(result.expect("valid authorization"), json!({}));
    }

    #[test]
    fn authorization_rejects_unknown_fields() {
        let error = authorize_plugin_invoke(json!({
            "plugin_id": "semantic",
            "export_id": "semantic.search",
            "input": {},
            "admin": true
        }))
        .expect_err("unknown field must fail");

        assert!(error.to_string().contains("invalid_host_capability_input"));
    }
}
