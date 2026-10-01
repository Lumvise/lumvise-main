use chrono::Utc;
use lumvise_contracts::ObsidianSyncRequestV1;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::{KnowledgeArtifact, cultivation, events, projection, storage};

pub(crate) fn manifest(context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let output = context.host_call("storage.semantic", json!({"operation": "project_roots"}))?;
    let roots = output["project_roots"]
        .as_array()
        .ok_or_else(|| invalid_http(&output, "storage.semantic project_roots array"))?;
    let projects = roots
        .iter()
        .filter_map(Value::as_str)
        .map(|source_id| {
            json!({"sourceId": source_id, "displayName": project_name(source_id),
                "status": "ready", "granularities": ["file"]})
        })
        .collect::<Vec<_>>();
    Ok(json!({"generatedAt": Utc::now().to_rfc3339(), "projects": projects}))
}

pub(crate) fn setup(input: &Value) -> Value {
    let source_id = source_parameter(input).unwrap_or_default();
    json!({
        "schemaVersion": 1, "vaultId": null, "origin": "lumvise-mcp",
        "mode": "mixed-vault", "createdAt": Utc::now().to_rfc3339(), "mcpBaseUrl": "",
        "projects": [{"schemaVersion": 1, "sourceId": source_id,
            "displayName": project_name(&source_id), "granularity": "file"}]
    })
}

pub(crate) fn export(input: &Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    projection::project(&required_source_parameter(input)?, context)
}

pub(crate) fn page(input: &Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    projection::page(
        &required_source_parameter(input)?,
        &required_parameter_alias(input, "elementId", "element_id")?,
        context,
    )
}

pub(crate) fn sync(
    input: &Value,
    context: &mut PluginContext<'_>,
    projection_cache: &projection::ProjectProjectionCache,
) -> Result<Value, PluginError> {
    let request = parse_sync_request(input)?;
    projection::sync(
        &request.source_id,
        &json!(request.content_hashes),
        request.applied_revision,
        context,
        projection_cache,
    )
}

fn parse_sync_request(input: &Value) -> Result<ObsidianSyncRequestV1, PluginError> {
    let body = input.get("body").cloned().unwrap_or(Value::Null);
    ObsidianSyncRequestV1::from_value(body).map_err(|error| {
        PluginError::new(
            "invalid_obsidian_sync_input",
            format!("invalid Obsidian sync input `{input}`; expected schema v1 body: {error}"),
            false,
        )
    })
}

pub(crate) fn write(input: Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let body = input.get("body").cloned().unwrap_or(Value::Null);
    crate::create(body, context)
}
/// Adapts a signed HTTP envelope to the shared create contract.
pub(crate) fn create_artifact(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::create(http_body(input), context)
}

/// Adapts a signed HTTP envelope to the shared update contract.
pub(crate) fn update_artifact(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::update(http_body(input), context)
}

/// Adapts a signed HTTP envelope to the shared delete contract.
pub(crate) fn delete_artifact(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    crate::delete(http_body(input), context)
}
fn http_body(input: Value) -> Value {
    input.get("body").cloned().unwrap_or(Value::Null)
}

pub(crate) fn resolve_target(
    input: &Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let body = input.get("body").unwrap_or(&Value::Null);
    let semantic_element_id = body["semantic_element_id"]
        .as_str()
        .ok_or_else(|| invalid_http(input, "JSON body semantic_element_id"))?;
    let element = storage::required_semantic_element(context, semantic_element_id)?;
    Ok(json!({"element": storage::public_semantic_element(&element)}))
}

pub(crate) fn c4(input: &Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let body = input.get("body").cloned().unwrap_or(Value::Null);
    let project_root = body["project_root"]
        .as_str()
        .ok_or_else(|| invalid_http(input, "JSON body project_root"))?
        .to_owned();
    let response = cultivation::ensure_c4(body, context)?;
    if response["status"] != "ready" {
        return Ok(c4_incomplete_response(&response));
    }
    let artifact: KnowledgeArtifact = serde_json::from_value(response["artifact"].clone())
        .map_err(|error| {
            PluginError::new(
                "invalid_knowledge_projection",
                format!(
                    "invalid C4 artifact `{}`; expected Knowledge artifact: {error}",
                    response["artifact"]
                ),
                false,
            )
        })?;
    let projected = crate::obsidian_reports::project(&project_root, &artifact);
    let mut ready = json!({"status": "ready", "created": response["created"],
        "artifactId": artifact.artifact_id, "targetPath": projected["pathHint"]});
    if let Some(failed) = response.get("failed_element_ids") {
        ready["failedElementIds"] = failed.clone();
    }
    Ok(ready)
}

fn c4_incomplete_response(response: &Value) -> Value {
    if response["status"] == "pending" {
        return json!({"status": "pending", "requestId": response["request_id"],
            "targetFingerprint": response["target_fingerprint"],
            "missingElementIds": response["missing_element_ids"],
            "pendingElementIds": response["pending_element_ids"],
            "failedElementIds": response["failed_element_ids"],
            "artifactId": response["artifact_id"], "targetPath": response["target_path"]});
    }
    json!({"status": "not_ready", "reasonCode": response["reason_code"],
        "reason": response["reason"], "targetFingerprint": response["target_fingerprint"],
        "artifactId": response["artifact_id"], "targetPath": response["target_path"]})
}

pub(crate) fn c4_debug(
    input: &Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let mut request = query_as_object(input);
    if let Some(root) = parameter(input, "project_root") {
        request["project_root"] = json!(root);
    }
    cultivation::debug_c4(request, context)
}

pub(crate) fn c4_action(input: &Value) -> Value {
    json!({
        "action": "ensure_c4_nucleus", "method": "POST",
        "path": "/api/knowledge/c4-nucleus", "query": input["query"]
    })
}

pub(crate) fn events(input: &Value, context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    let cursor = input["cursor"]
        .as_str()
        .or_else(|| input["query"]["cursor"].as_str());
    let max_events = input["max_events"]
        .as_u64()
        .or_else(|| input["query"]["max_events"].as_str()?.parse().ok())
        .unwrap_or(20) as usize;
    if !(1..=100).contains(&max_events) {
        return Err(invalid_http(input, "max_events integer from 1 through 100"));
    }
    events::live_events(cursor, max_events, context)
}

pub(crate) fn projection_artifacts(context: &mut PluginContext<'_>) -> Result<Value, PluginError> {
    Ok(json!({"artifacts": storage::list_all_knowledge(context)?}))
}

fn parameter(input: &Value, key: &str) -> Option<String> {
    input["query"][key]
        .as_str()
        .or_else(|| input["path_parameters"][key].as_str())
        .map(str::to_owned)
}

fn source_parameter(input: &Value) -> Option<String> {
    parameter(input, "sourceId").or_else(|| parameter(input, "source_id"))
}

fn required_source_parameter(input: &Value) -> Result<String, PluginError> {
    source_parameter(input)
        .ok_or_else(|| invalid_http(input, "query parameter `sourceId` or `source_id`"))
}

fn required_parameter_alias(
    input: &Value,
    camel_case: &str,
    snake_case: &str,
) -> Result<String, PluginError> {
    parameter(input, camel_case)
        .or_else(|| parameter(input, snake_case))
        .ok_or_else(|| {
            invalid_http(
                input,
                &format!("query parameter `{camel_case}` or `{snake_case}`"),
            )
        })
}

fn query_as_object(input: &Value) -> Value {
    input.get("query").cloned().unwrap_or_else(|| json!({}))
}

fn project_name(project_root: &str) -> &str {
    project_root
        .trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(project_root)
}

fn invalid_http(input: &Value, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_knowledge_http_input",
        format!("invalid Knowledge HTTP input `{input}`; expected {expected}"),
        false,
    )
}
