use std::collections::BTreeMap;

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::KnowledgeArtifact;

pub(crate) const VECTORS: &str = "knowledge_vectors";
pub(crate) const CULTIVATION_RUNS: &str = "knowledge_cultivation_runs";
pub(crate) const LIVE_EVENTS: &str = "knowledge_live_events";

const CAPABILITY: &str = "storage.plugin";
const SEMANTIC_CAPABILITY: &str = "storage.semantic";
const PAGE_LIMIT: usize = 500;

pub(crate) fn put<T: Serialize>(
    context: &mut PluginContext<'_>,
    table: &str,
    row_key: &str,
    value: &T,
) -> Result<(), PluginError> {
    ensure(context, table)?;
    context.host_call(
        CAPABILITY,
        json!({"operation": "put_row", "table_name": table,
            "row_key": row_key, "value": value}),
    )?;
    Ok(())
}

pub(crate) fn get<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    table: &str,
    row_key: &str,
) -> Result<Option<T>, PluginError> {
    ensure(context, table)?;
    let output = context.host_call(
        CAPABILITY,
        json!({"operation": "get_row", "table_name": table, "row_key": row_key}),
    )?;
    output
        .get("row")
        .filter(|row| !row.is_null())
        .map(|row| decode(row.get("value").unwrap_or(&Value::Null), table))
        .transpose()
}

pub(crate) fn list<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    table: &str,
    key_prefix: Option<&str>,
) -> Result<Vec<T>, PluginError> {
    list_sql(context, table, key_prefix)
}

pub(crate) fn list_project_knowledge(
    context: &mut PluginContext<'_>,
    project_root: &str,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "project_artifacts",
            "project_root": project_root, "artifact_namespace": "knowledge"}),
    )?;
    let artifacts = output
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("semantic project snapshot artifacts", &output))?;
    let mut artifacts = decode_graph_knowledge_artifacts(artifacts)?;
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    Ok(artifacts)
}

pub(crate) fn elements_by_ids(
    context: &mut PluginContext<'_>,
    project_root: &str,
    element_ids: &std::collections::HashSet<String>,
) -> Result<Vec<crate::semantic_context::SemanticElement>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "elements_by_ids_including_inactive",
            "project_root": project_root, "semantic_element_ids": element_ids}),
    )?;
    output
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("semantic targeted elements", &output))
        .and_then(|elements| {
            elements
                .iter()
                .map(|element| {
                    serde_json::from_value(element.clone()).map_err(|error| {
                        invalid_response("semantic targeted element", &json!(error.to_string()))
                    })
                })
                .collect()
        })
}

/// Reads one semantic element's identity directly via `storage.semantic`,
/// never through `plugin.invoke`/`get_semantic_tree`.
pub(crate) fn semantic_element(
    context: &mut PluginContext<'_>,
    semantic_element_id: &str,
) -> Result<Option<Value>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "element", "semantic_element_id": semantic_element_id}),
    )?;
    let element = output
        .get("element")
        .ok_or_else(|| invalid_response("semantic element", &output))?;
    Ok((!element.is_null()).then(|| element.clone()))
}

/// Reads one semantic element's identity, failing with the same missing-element
/// error surface Knowledge's create/update/list/get-element paths have always
/// reported.
pub(crate) fn required_semantic_element(
    context: &mut PluginContext<'_>,
    semantic_element_id: &str,
) -> Result<Value, PluginError> {
    semantic_element(context, semantic_element_id)?.ok_or_else(|| {
        PluginError::new(
            "semantic_element_not_found",
            format!(
                "semantic element `{semantic_element_id}` is missing; expected compiled Semantic identity record"
            ),
            false,
        )
    })
}

/// Projects a full `storage.semantic` element record down to the signed
/// `get_semantic_element`/`resolve_target` export shape. The shape is intentionally
/// limited to stable identity fields and must not widen.
pub(crate) fn public_semantic_element(element: &Value) -> Value {
    json!({
        "semantic_element_id": element["semantic_element_id"],
        "element_kind": element["element_kind"],
        "name": element["name"],
        "path": element["path"],
        "parent_element_id": element["parent_element_id"],
        "start_line": element["start_line"],
        "end_line": element["end_line"],
    })
}

/// Bounded cross-project identity lookup: unions exact matches on raw content
/// fingerprint, normalized `(kind, name)`, and normalized `(kind, file name)`.
pub(crate) fn candidate_source_elements(
    context: &mut PluginContext<'_>,
    content_fingerprints: &std::collections::HashSet<String>,
    kind_name_keys: &std::collections::HashSet<String>,
    kind_file_name_keys: &std::collections::HashSet<String>,
) -> Result<Vec<crate::semantic_context::SemanticElement>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "candidate_source_elements",
            "content_fingerprints": content_fingerprints, "kind_name_keys": kind_name_keys,
            "kind_file_name_keys": kind_file_name_keys}),
    )?;
    output
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("semantic candidate source elements", &output))
        .and_then(|elements| {
            elements
                .iter()
                .map(|element| {
                    serde_json::from_value(element.clone()).map_err(|error| {
                        invalid_response(
                            "semantic candidate source element",
                            &json!(error.to_string()),
                        )
                    })
                })
                .collect()
        })
}

pub(crate) fn selective_subgraph(
    context: &mut PluginContext<'_>,
    project_root: &str,
    root_element_id: &str,
) -> Result<Option<crate::semantic_context::SelectiveSubgraph>, PluginError> {
    crate::semantic_context::load_selective_subgraph(context, project_root, root_element_id)
}

pub(crate) fn decode_selective_knowledge_artifacts(
    artifacts: &[crate::semantic_context::SemanticArtifact],
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    artifacts
        .iter()
        .filter(|artifact| artifact.metadata["knowledge"].is_object())
        .map(|artifact| {
            let knowledge = &artifact.metadata["knowledge"];
            decode(
                &json!({
                    "artifact_id": artifact.artifact_id,
                    "semantic_element_id": artifact.semantic_element_id,
                    "knowledge_type": artifact.artifact_kind,
                    "title": artifact.title,
                    "content": artifact.content,
                    "tags": knowledge["tags"],
                    "metadata": knowledge["metadata"],
                    "dependencies": artifact.dependencies,
                    "project_root": knowledge["project_root"]
                }),
                "selective semantic artifacts",
            )
        })
        .collect()
}

pub(crate) fn put_knowledge(
    context: &mut PluginContext<'_>,
    artifact: &KnowledgeArtifact,
) -> Result<(), PluginError> {
    put_graph_artifact(context, artifact)
}

pub(crate) fn get_knowledge(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<Option<KnowledgeArtifact>, PluginError> {
    get_graph_artifact(context, artifact_id)
}

pub(crate) fn remove_knowledge(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<bool, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "remove_artifact", "artifact_id": artifact_id}),
    )?;
    output["removed"]
        .as_bool()
        .ok_or_else(|| invalid_response("semantic Knowledge artifact removal boolean", &output))
}

pub(crate) fn artifacts_for_elements(
    context: &mut PluginContext<'_>,
    semantic_element_ids: &std::collections::HashSet<String>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    if semantic_element_ids.is_empty() {
        return Ok(Vec::new());
    }
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "artifacts_for_elements",
            "semantic_element_ids": semantic_element_ids}),
    )?;
    decode_graph_knowledge_list(&output)
}

pub(crate) fn list_knowledge_dependents(
    context: &mut PluginContext<'_>,
    target_kind: &str,
    target_id: &str,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({
            "operation": "artifact_dependents",
            "target_kind": target_kind,
            "target_id": target_id
        }),
    )?;
    decode_graph_knowledge_list(&output)
}

pub(crate) fn list_all_knowledge(
    context: &mut PluginContext<'_>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let roots = semantic_project_roots(context)?;
    let mut artifacts = BTreeMap::new();
    for root in roots {
        for artifact in list_project_knowledge(context, &root)? {
            artifacts.insert(artifact.artifact_id.clone(), artifact);
        }
    }
    Ok(artifacts.into_values().collect())
}

fn list_sql<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    table: &str,
    key_prefix: Option<&str>,
) -> Result<Vec<T>, PluginError> {
    ensure(context, table)?;
    let mut records = Vec::new();
    let mut after_key: Option<String> = None;
    loop {
        let output = list_page(context, table, key_prefix, after_key.as_deref())?;
        append_rows(&mut records, table, &output)?;
        let next = output
            .get("next_after_key")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if next.is_some() && next == after_key {
            return Err(repeated_cursor(table, &next));
        }
        after_key = next;
        if after_key.is_none() {
            return Ok(records);
        }
    }
}

pub(crate) fn trim_rows_by_key(
    context: &mut PluginContext<'_>,
    table: &str,
    retained_rows: usize,
) -> Result<(), PluginError> {
    ensure(context, table)?;
    context.host_call(
        CAPABILITY,
        json!({"operation": "trim_rows_by_key", "table_name": table,
            "retained_rows": retained_rows}),
    )?;
    Ok(())
}

pub(crate) fn delete(
    context: &mut PluginContext<'_>,
    table: &str,
    row_key: &str,
) -> Result<(), PluginError> {
    ensure(context, table)?;
    context.host_call(
        CAPABILITY,
        json!({"operation": "mutate_rows", "mutations": [{
            "operation": "delete", "table_name": table, "row_key": row_key
        }]}),
    )?;
    Ok(())
}

pub(crate) fn put_rows<T: Serialize>(
    context: &mut PluginContext<'_>,
    table: &str,
    rows: &[(String, T)],
) -> Result<(), PluginError> {
    if rows.is_empty() {
        return Ok(());
    }
    ensure(context, table)?;
    let mutations = rows
        .iter()
        .map(|(row_key, value)| {
            json!({
                "operation": "put", "table_name": table, "row_key": row_key, "value": value
            })
        })
        .collect::<Vec<_>>();
    context.host_call(
        CAPABILITY,
        json!({"operation": "mutate_rows", "mutations": mutations}),
    )?;
    Ok(())
}

pub(crate) fn page<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    table: &str,
    after_key: Option<&str>,
    limit: usize,
) -> Result<Vec<T>, PluginError> {
    ensure(context, table)?;
    let output = list_page(context, table, None, after_key)?;
    output["rows"]
        .as_array()
        .ok_or_else(|| invalid_response(table, &output))?
        .iter()
        .take(limit)
        .map(|row| decode(&row["value"], table))
        .collect()
}

fn put_graph_artifact<T: Serialize>(
    context: &mut PluginContext<'_>,
    value: &T,
) -> Result<(), PluginError> {
    let knowledge = serde_json::to_value(value).map_err(encode_error)?;
    let artifact = json!({
        "artifact_id": knowledge["artifact_id"],
        "semantic_element_id": knowledge["semantic_element_id"],
        "artifact_kind": knowledge["knowledge_type"],
        "title": knowledge["title"],
        "content_ref": null,
        "content": knowledge["content"],
        "searchable_text": null,
        "content_size_bytes": null,
        "dependencies": knowledge["dependencies"],
        "metadata": {"knowledge": {
            "tags": knowledge["tags"],
            "metadata": knowledge["metadata"],
            "path": knowledge["path"],
            "project_root": knowledge["project_root"]
        }}
    });
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "upsert_artifact", "artifact": artifact,
            "media_type": "text/markdown; charset=utf-8"}),
    )?;
    Ok(())
}

fn get_graph_artifact<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<Option<T>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "artifact", "artifact_id": artifact_id}),
    )?;
    output
        .get("artifact")
        .filter(|artifact| !artifact.is_null())
        .filter(|artifact| is_knowledge_artifact(artifact))
        .map(decode_graph_artifact)
        .transpose()
}

fn decode_graph_knowledge_list(output: &Value) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let artifacts = output
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("semantic Knowledge artifacts", output))?;
    decode_graph_knowledge_artifacts(artifacts)
}

pub(crate) fn decode_graph_knowledge_artifacts(
    artifacts: &[Value],
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    artifacts
        .iter()
        .filter(|artifact| is_knowledge_artifact(artifact))
        .map(decode_graph_artifact)
        .collect()
}

fn semantic_project_roots(context: &mut PluginContext<'_>) -> Result<Vec<String>, PluginError> {
    let output = context.host_call(SEMANTIC_CAPABILITY, json!({"operation": "project_roots"}))?;
    output
        .get("project_roots")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("semantic project roots", &output))?
        .iter()
        .map(|root| decode(root, "semantic project roots"))
        .collect()
}

fn is_knowledge_artifact(artifact: &Value) -> bool {
    artifact["metadata"]["knowledge"].is_object()
}

fn decode_graph_artifact<T: DeserializeOwned>(artifact: &Value) -> Result<T, PluginError> {
    let knowledge = &artifact["metadata"]["knowledge"];
    decode(
        &json!({
            "artifact_id": artifact["artifact_id"],
            "semantic_element_id": artifact["semantic_element_id"],
            "knowledge_type": artifact["artifact_kind"],
            "title": artifact["title"],
            "content": artifact["content"],
            "tags": knowledge["tags"],
            "metadata": knowledge["metadata"],
            "dependencies": artifact["dependencies"],
            "path": knowledge["path"],
            "project_root": knowledge["project_root"]
        }),
        "semantic Knowledge artifacts",
    )
}

fn encode_error(error: serde_json::Error) -> PluginError {
    PluginError::new(
        "invalid_knowledge_storage",
        format!("failed to encode Knowledge artifact; expected serializable record: {error}"),
        false,
    )
}

fn list_page(
    context: &mut PluginContext<'_>,
    table: &str,
    key_prefix: Option<&str>,
    after_key: Option<&str>,
) -> Result<Value, PluginError> {
    let mut input = json!({
        "operation": "list_rows", "table_name": table, "limit": PAGE_LIMIT
    });
    if let Some(prefix) = key_prefix {
        input["key_prefix"] = json!(prefix);
    }
    if let Some(cursor) = after_key {
        input["after_key"] = json!(cursor);
    }
    context.host_call(CAPABILITY, input)
}

fn append_rows<T: DeserializeOwned>(
    records: &mut Vec<T>,
    table: &str,
    output: &Value,
) -> Result<(), PluginError> {
    let rows = output
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response(table, output))?;
    records.extend(
        rows.iter()
            .map(|row| decode(row.get("value").unwrap_or(&Value::Null), table))
            .collect::<Result<Vec<T>, _>>()?,
    );
    Ok(())
}

fn ensure(context: &mut PluginContext<'_>, table: &str) -> Result<(), PluginError> {
    context.host_call(
        CAPABILITY,
        json!({"operation": "ensure_table", "table_name": table,
            "schema": {"type": "object"}}),
    )?;
    Ok(())
}

fn decode<T: DeserializeOwned>(value: &Value, table: &str) -> Result<T, PluginError> {
    serde_json::from_value(value.clone()).map_err(|error| {
        PluginError::new(
            "invalid_knowledge_storage",
            format!("invalid stored value `{value}` in table `{table}`; expected Knowledge record: {error}"),
            false,
        )
    })
}

fn invalid_response(table: &str, value: &Value) -> PluginError {
    PluginError::new(
        "invalid_storage_response",
        format!(
            "invalid storage.plugin response `{value}` for table `{table}`; expected rows array"
        ),
        false,
    )
}

fn repeated_cursor(table: &str, cursor: &Option<String>) -> PluginError {
    PluginError::new(
        "invalid_storage_response",
        format!(
            "storage.plugin repeated cursor `{cursor:?}` for table `{table}`; expected advancing next_after_key"
        ),
        false,
    )
}
