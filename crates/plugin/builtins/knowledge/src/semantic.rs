use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CAPABILITY: &str = "plugin.invoke";
const SEMANTIC_PLUGIN_ID: &str = "builtin.semantic";

#[derive(Deserialize, Serialize)]
struct SemanticSearchInput {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    element_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_inactive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
}

pub(crate) fn search_elements(
    context: &mut PluginContext<'_>,
    input: Value,
) -> Result<Value, PluginError> {
    let semantic_input = normalized_search_input(input)?;
    let output = invoke_public(context, "search_semantic_elements", semantic_input)?;
    let elements = output["results"]
        .as_array()
        .ok_or_else(|| invalid_output("search_semantic_elements", &output))?
        .iter()
        .filter_map(|result| result.get("element").cloned())
        .collect::<Vec<_>>();
    Ok(json!({"elements": elements, "mode": output["mode"]}))
}

fn normalized_search_input(input: Value) -> Result<Value, PluginError> {
    let request: SemanticSearchInput = serde_json::from_value(input.clone()).map_err(|error| {
        PluginError::new(
            "invalid_input",
            format!("invalid Knowledge semantic search `{input}`; expected query and optional public Semantic filters: {error}"),
            false,
        )
    })?;
    serde_json::to_value(request).map_err(|error| {
        PluginError::new(
            "invalid_input",
            format!("Knowledge semantic search normalization failed; expected serializable public filters: {error}"),
            false,
        )
    })
}

pub(crate) fn invoke_public(
    context: &mut PluginContext<'_>,
    export_id: &str,
    input: Value,
) -> Result<Value, PluginError> {
    let output = context.host_call(
        CAPABILITY,
        json!({"plugin_id": SEMANTIC_PLUGIN_ID, "export_id": export_id, "input": input}),
    )?;
    output
        .get("output")
        .cloned()
        .ok_or_else(|| invalid_output(export_id, &output))
}

fn invalid_output(export_id: &str, output: &Value) -> PluginError {
    PluginError::new(
        "invalid_plugin_invoke_response",
        format!(
            "invalid plugin.invoke response `{output}` for `{SEMANTIC_PLUGIN_ID}/{export_id}`; expected object field `output`"
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_identity_is_consumed_before_semantic_plugin_invocation() {
        let normalized = normalized_search_input(json!({
            "query":"architecture", "project_root":"/project",
            "plugin_id":"builtin.assistant", "session_id":"session-7"
        }))
        .expect("normalize scoped semantic search");

        assert_eq!(
            normalized,
            json!({"query":"architecture", "project_root":"/project"})
        );
    }
}
