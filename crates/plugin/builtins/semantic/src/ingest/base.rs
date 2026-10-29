//! Captures explicit current write scopes; callers own durable editing intent and retries.
//! Call `observe_index_base` before preparing input. This read establishes no checkout equivalence.
use crate::{parse, require_non_empty, serialize_response};
use lumvise_contracts::{SemanticIndexObservation, SemanticSelectionExpectation};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexBaseRequest {
    project_root: String,
    provider_instance_id: String,
    paths: Vec<String>,
    artifact_ids: Vec<String>,
}

/// Captures existing scoped membership for a producer that has verified its editing base.
/// Example: call with `{"project_root":"/project","provider_instance_id":"agents","paths":["space"],"artifact_ids":[]}`.
pub(crate) fn observe_index_base(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: IndexBaseRequest = parse(input, "an explicit provider/path/artifact scope")?;
    request.validate()?;
    let project_id = read_index_base_project_id(context, &request.project_root)?;
    let expected = request.membership(context)?;
    serialize_response(SemanticIndexObservation {
        project_id,
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected,
    })
}

impl IndexBaseRequest {
    fn validate(&self) -> Result<(), PluginError> {
        require_non_empty(&self.project_root, "project_root")?;
        require_non_empty(&self.provider_instance_id, "provider_instance_id")?;
        for path in &self.paths {
            require_non_empty(path, "paths entry")?;
        }
        for id in &self.artifact_ids {
            require_non_empty(id, "artifact_ids entry")?;
        }
        Ok(())
    }

    fn membership(
        &self,
        context: &mut PluginContext<'_>,
    ) -> Result<SemanticSelectionExpectation, PluginError> {
        let output = context.host_call("storage.semantic", json!({
            "operation":"revision_selection_members", "query":{
                "project_root":self.project_root, "selection_id":"working",
                "structure_coverage":{"kind":"partition", "partition":{
                    "project_root":self.project_root, "provider_instance_id":self.provider_instance_id,
                    "replace_paths":self.paths}}, "artifact_ids":self.artifact_ids}}))?;
        decode_index_base_membership(&output)
    }
}

fn read_index_base_project_id(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<String, PluginError> {
    let output = context.host_call(
        "storage.semantic",
        json!({"operation":"project_identity", "project_root":root}),
    )?;
    output["identity"]["project_id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_index_base_output(&output, "a nonempty durable project UUID"))
}

fn decode_index_base_membership(
    output: &Value,
) -> Result<SemanticSelectionExpectation, PluginError> {
    let members = output
        .get("membership")
        .ok_or_else(|| invalid_index_base_output(output, "scoped membership or null"))?;
    if members.is_null() {
        return Ok(SemanticSelectionExpectation::default());
    }
    require_working_index_base(members)?;
    serde_json::from_value(members["expected"].clone()).map_err(|error| {
        invalid_index_base_output(
            output,
            &format!("exact scoped revision membership: {error}"),
        )
    })
}

fn require_working_index_base(members: &Value) -> Result<(), PluginError> {
    if members["selection"]["selection_id"] == "working"
        && members["selection"]["source_context"] == "local"
    {
        return Ok(());
    }
    Err(invalid_index_base_output(
        members,
        "working selection in the local source context",
    ))
}

fn invalid_index_base_output(output: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_semantic_storage",
        format!("index base `{output}`; expected {expected}"),
        false,
    )
}
