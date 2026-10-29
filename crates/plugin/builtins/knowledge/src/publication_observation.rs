//! Reads the existing publication token; Knowledge never reconstructs selection policy.
use lumvise_contracts::{SemanticIndexObservation, SemanticSelectionExpectation};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

/// Reads the caller's complete declared write scope before preparing input.
/// Example: `read(context, root, coverage, &artifact_ids)`.
pub(crate) fn read(
    context: &mut PluginContext<'_>,
    root: &str,
    coverage: Value,
    artifact_ids: &[String],
) -> Result<SemanticIndexObservation, PluginError> {
    let project_id = project_id(context, root)?;
    let members = context.host_call("storage.semantic", json!({"operation":"revision_selection_members", "query":{
        "project_root":root, "selection_id":"working", "structure_coverage":coverage, "artifact_ids":artifact_ids
    }}))?;
    Ok(SemanticIndexObservation {
        project_id,
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected: expectation(&members)?,
    })
}

/// Reads durable lineage without interpreting checkout paths.
/// Example: `project_id(context, root)` before preparing revision identities.
pub(crate) fn project_id(
    context: &mut PluginContext<'_>,
    root: &str,
) -> Result<String, PluginError> {
    let identity = context.host_call(
        "storage.semantic",
        json!({"operation":"project_identity", "project_root":root}),
    )?;
    let project_id = identity["identity"]["project_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid(&identity, "a non-empty durable project lineage"))?;
    Ok(project_id.into())
}

fn expectation(output: &Value) -> Result<SemanticSelectionExpectation, PluginError> {
    let membership = output
        .get("membership")
        .ok_or_else(|| invalid(output, "scoped revision membership or null"))?;
    if membership.is_null() {
        return Ok(SemanticSelectionExpectation::default());
    }
    if membership["selection"]["selection_id"] != "working"
        || membership["selection"]["source_context"] != "local"
    {
        return Err(invalid(
            output,
            "working selection in the local source context",
        ));
    }
    crate::parse(
        membership["expected"].clone(),
        "exact scoped revision membership",
    )
}

fn invalid(output: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_knowledge_observation",
        format!("invalid publication observation `{output}`; expected {expected}"),
        false,
    )
}
