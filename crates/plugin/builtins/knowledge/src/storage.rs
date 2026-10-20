use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::KnowledgeArtifact;

pub(crate) const VECTORS: &str = "knowledge_vectors";
pub(crate) const CULTIVATION_RUNS: &str = "knowledge_cultivation_runs";
pub(crate) const LIVE_EVENTS: &str = "knowledge_live_events";
const RETIRED_KIND_BATCH: usize = 200;

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
    semantic_elements_by_ids(
        context,
        project_root,
        element_ids,
        "elements_by_ids_including_inactive",
    )
}

fn active_elements_by_ids(
    context: &mut PluginContext<'_>,
    project_root: &str,
    element_ids: &HashSet<String>,
) -> Result<Vec<crate::semantic_context::SemanticElement>, PluginError> {
    semantic_elements_by_ids(context, project_root, element_ids, "elements_by_ids")
}

fn semantic_elements_by_ids(
    context: &mut PluginContext<'_>,
    project_root: &str,
    element_ids: &HashSet<String>,
    operation: &str,
) -> Result<Vec<crate::semantic_context::SemanticElement>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": operation,
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
            let mut record = json!({
                "artifact_id": artifact.artifact_id,
                "semantic_element_id": artifact.semantic_element_id,
                "knowledge_type": current_kind(&artifact.artifact_kind),
                "title": artifact.title,
                "content": artifact.content,
                "tags": knowledge["tags"],
                "metadata": knowledge["metadata"],
                "dependencies": artifact.dependencies,
                "project_root": knowledge["project_root"]
            });
            merge_lifecycle(&mut record, knowledge);
            decode(&record, "selective semantic artifacts")
        })
        .collect()
}

/// The only Knowledge write path: stamps revision and timestamps from the stored
/// record, then persists and returns the stamped record.
pub(crate) fn put_knowledge(
    context: &mut PluginContext<'_>,
    artifact: &KnowledgeArtifact,
) -> Result<KnowledgeArtifact, PluginError> {
    let stored = raw_graph_artifact(context, &artifact.artifact_id)?;
    validate_knowledge_owner(artifact, stored.as_ref())?;
    let previous = stored
        .as_ref()
        .map(decode_graph_artifact::<KnowledgeArtifact>)
        .transpose()?
        .filter(|previous| previous.project_root == artifact.project_root);
    let mut stamped = artifact.clone();
    crate::artifact::stamp(
        &mut stamped,
        previous.as_ref().map(|previous| &previous.lifecycle),
        Utc::now(),
    );
    stamped.artifact_id = put_graph_artifact(context, &stamped)?;
    Ok(stamped)
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

fn put_graph_artifact(
    context: &mut PluginContext<'_>,
    value: &KnowledgeArtifact,
) -> Result<String, PluginError> {
    let knowledge = serde_json::to_value(value).map_err(encode_error)?;
    let lifecycle = serde_json::to_value(&value.lifecycle).map_err(encode_error)?;
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
            "project_root": knowledge["project_root"],
            "lifecycle": lifecycle
        }}
    });
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "upsert_artifact", "artifact": artifact,
            "media_type": "text/markdown; charset=utf-8"}),
    )?;
    output["artifact_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_response("stored artifact identity", &output))
}

pub(crate) fn has_knowledge_record(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<bool, PluginError> {
    Ok(raw_graph_artifact(context, artifact_id)?
        .as_ref()
        .is_some_and(is_knowledge_artifact))
}

fn get_graph_artifact<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<Option<T>, PluginError> {
    raw_graph_artifact(context, artifact_id)?
        .as_ref()
        .filter(|artifact| is_knowledge_artifact(artifact))
        .map(decode_graph_artifact)
        .transpose()
}

fn validate_knowledge_owner(
    artifact: &KnowledgeArtifact,
    stored: Option<&Value>,
) -> Result<(), PluginError> {
    if let Some(stored) = stored.filter(|stored| !is_knowledge_artifact(stored)) {
        return Err(PluginError::new(
            "knowledge_id_conflict",
            format!(
                "artifact ID `{}` belongs to `{}`; expected an unused ID or an existing Knowledge artifact",
                artifact.artifact_id, stored["artifact_kind"]
            ),
            false,
        ));
    }
    Ok(())
}

fn raw_graph_artifact(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<Option<Value>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "artifact", "artifact_id": artifact_id}),
    )?;
    Ok(output
        .get("artifact")
        .filter(|artifact| !artifact.is_null())
        .cloned())
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
    let mut record = json!({
        "artifact_id": artifact["artifact_id"],
        "semantic_element_id": artifact["semantic_element_id"],
        "knowledge_type": current_kind(artifact["artifact_kind"].as_str().unwrap_or_default()),
        "title": artifact["title"],
        "content": artifact["content"],
        "tags": knowledge["tags"],
        "metadata": knowledge["metadata"],
        "dependencies": artifact["dependencies"],
        "path": knowledge["path"],
        "project_root": knowledge["project_root"]
    });
    merge_lifecycle(&mut record, knowledge);
    decode(&record, "semantic Knowledge artifacts")
}

/// Flattens the stored lifecycle envelope into the record it decodes from. An
/// envelope without `lifecycle` leaves the record to decode as the default lifecycle.
fn merge_lifecycle(record: &mut Value, knowledge: &Value) {
    if let (Some(record), Some(lifecycle)) =
        (record.as_object_mut(), knowledge["lifecycle"].as_object())
    {
        record.extend(lifecycle.clone());
    }
}

/// Rows written before `manual_note` and `derived_summary` were retired, and stores
/// not converted yet, still carry the old kind; they read as its replacement.
fn current_kind(stored: &str) -> &str {
    match stored {
        "manual_note" => "annotation",
        "derived_summary" => "report",
        other => other,
    }
}

/// Set once no convertible retired kinds remain, or once conversion fails, so polling settles.
static RETIRED_KINDS_SETTLED: AtomicBool = AtomicBool::new(false);

/// Runs one conversion batch per poll until no convertible retired rows remain. Rows with
/// inactive owners stay stored because writes require an active owner; reads use `current_kind`.
/// A store that cannot be written must not fail or retry the poll.
pub(crate) fn settle_retired_kinds(context: &mut PluginContext<'_>) {
    if RETIRED_KINDS_SETTLED.load(Ordering::Relaxed) {
        return;
    }
    match convert_retired_kinds(context, RETIRED_KIND_BATCH) {
        Ok(0) | Err(_) => RETIRED_KINDS_SETTLED.store(true, Ordering::Relaxed),
        Ok(_) => {}
    }
}

/// Rewrites up to `limit` stored rows of retired kinds. The rewrite does not stamp, so
/// revision and timestamps are kept.
pub(crate) fn convert_retired_kinds(
    context: &mut PluginContext<'_>,
    limit: usize,
) -> Result<usize, PluginError> {
    let mut rewritten = 0;
    for project_root in semantic_project_roots(context)? {
        if rewritten == limit {
            break;
        }
        let output = context.host_call(
            SEMANTIC_CAPABILITY,
            json!({"operation": "project_artifacts",
                "project_root": project_root, "artifact_namespace": "knowledge"}),
        )?;
        let rows = output
            .get("artifacts")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_response("semantic project snapshot artifacts", &output))?;
        rewritten +=
            convert_project_retired_kinds(context, &project_root, rows, limit - rewritten)?;
    }
    Ok(rewritten)
}

fn convert_project_retired_kinds(
    context: &mut PluginContext<'_>,
    project_root: &str,
    rows: &[Value],
    limit: usize,
) -> Result<usize, PluginError> {
    let artifacts = retired_knowledge_artifacts(rows)?;
    if artifacts.is_empty() {
        return Ok(0);
    }
    let owner_ids = artifacts
        .iter()
        .map(|artifact| artifact.semantic_element_id.clone())
        .collect::<HashSet<_>>();
    let active_owner_ids = active_elements_by_ids(context, project_root, &owner_ids)?
        .into_iter()
        .map(|element| element.semantic_element_id)
        .collect::<HashSet<_>>();
    rewrite_active_retired_artifacts(context, artifacts, &active_owner_ids, limit)
}

fn retired_knowledge_artifacts(rows: &[Value]) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    rows.iter()
        .filter(|row| {
            is_knowledge_artifact(row)
                && row["artifact_kind"]
                    .as_str()
                    .is_some_and(|kind| current_kind(kind) != kind)
        })
        .map(decode_graph_artifact)
        .collect()
}

fn rewrite_active_retired_artifacts(
    context: &mut PluginContext<'_>,
    artifacts: Vec<KnowledgeArtifact>,
    active_owner_ids: &HashSet<String>,
    limit: usize,
) -> Result<usize, PluginError> {
    let mut rewritten = 0;
    for artifact in artifacts {
        if rewritten == limit {
            break;
        }
        if !active_owner_ids.contains(&artifact.semantic_element_id) {
            continue;
        }
        put_graph_artifact(context, &artifact)?;
        rewritten += 1;
    }
    Ok(rewritten)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnowledgeKind;
    use lumvise_plugin_sdk::HostCallTransport;

    /// Raw graph rows of one project; upserts replace the row with the same artifact id.
    struct RawStore {
        rows: Vec<Value>,
        inactive_owner_ids: HashSet<String>,
        active_owner_lookups: usize,
    }

    impl HostCallTransport for RawStore {
        fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError> {
            match (capability_id, input["operation"].as_str()) {
                ("storage.semantic", Some("project_roots")) => {
                    Ok(json!({"project_roots": ["/repo"]}))
                }
                ("storage.semantic", Some("project_artifacts")) => {
                    Ok(json!({"artifacts": self.rows}))
                }
                ("storage.semantic", Some("elements_by_ids")) => {
                    self.active_owner_lookups += 1;
                    let elements = input["semantic_element_ids"]
                        .as_array()
                        .expect("semantic element ids")
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|id| !self.inactive_owner_ids.contains(*id))
                        .map(active_element_row)
                        .collect::<Vec<_>>();
                    Ok(json!({"elements": elements}))
                }
                ("storage.semantic", Some("upsert_artifact")) => {
                    let row = input["artifact"].clone();
                    self.rows
                        .retain(|stored| stored["artifact_id"] != row["artifact_id"]);
                    self.rows.push(row);
                    Ok(json!({"artifact_id": input["artifact"]["artifact_id"]}))
                }
                _ => Err(PluginError::unknown_capability(capability_id)),
            }
        }
    }

    fn row(artifact_id: &str, kind: &str) -> Value {
        json!({
            "artifact_id": artifact_id, "semantic_element_id": "file:src/a.rs",
            "artifact_kind": kind, "title": "Title", "content": "Body",
            "dependencies": [], "metadata": {"knowledge": {
                "tags": [], "metadata": {}, "path": null, "project_root": "/repo",
                "lifecycle": {"status": "resolved", "revision": 3,
                    "created_at": "2026-10-01T00:00:00+00:00",
                    "updated_at": "2026-10-02T00:00:00+00:00"}}}
        })
    }

    fn active_element_row(semantic_element_id: &str) -> Value {
        json!({
            "project_root": "/repo", "semantic_element_id": semantic_element_id,
            "semantic_source_id": "fixture", "path": "src/a.rs", "element_kind": "file",
            "name": "a.rs", "parent_element_id": null, "content_fingerprint": null,
            "start_line": null, "end_line": null, "lifecycle": "active", "metadata": {}
        })
    }

    #[test]
    fn retired_kinds_read_as_their_replacements_in_both_decoders() {
        for (stored, current) in [
            ("manual_note", KnowledgeKind::Annotation),
            ("derived_summary", KnowledgeKind::Report),
        ] {
            let graph: KnowledgeArtifact =
                decode_graph_artifact(&row("a", stored)).expect("graph decode");
            assert_eq!(graph.knowledge_type, current);

            let semantic: crate::semantic_context::SemanticArtifact =
                serde_json::from_value(json!({
                    "project_root": "/repo", "artifact_id": "a",
                    "semantic_element_id": "file:src/a.rs", "artifact_kind": stored,
                    "title": "Title", "content": "Body", "content_ref": null,
                    "searchable_text": null, "content_size_bytes": null,
                    "metadata": row("a", stored)["metadata"].clone()
                }))
                .expect("semantic artifact");
            let selective = decode_selective_knowledge_artifacts(&[semantic]).expect("selective");
            assert_eq!(selective[0].knowledge_type, current);
        }
    }

    #[test]
    fn convert_retired_kinds_rewrites_in_batches_without_stamping() {
        let mut store = RawStore {
            rows: vec![
                row("manual", "manual_note"),
                row("summary", "derived_summary"),
                row("definition", "definition"),
            ],
            inactive_owner_ids: HashSet::new(),
            active_owner_lookups: 0,
        };
        let mut context = PluginContext::for_test(&mut store);
        assert_eq!(
            convert_retired_kinds(&mut context, 1).expect("first batch"),
            1
        );
        assert_eq!(
            convert_retired_kinds(&mut context, 1).expect("second batch"),
            1
        );
        assert_eq!(convert_retired_kinds(&mut context, 1).expect("settled"), 0);

        let kind_of = |id: &str| {
            store
                .rows
                .iter()
                .find(|stored| stored["artifact_id"] == id)
                .expect("row")
        };
        assert_eq!(kind_of("manual")["artifact_kind"], "annotation");
        assert_eq!(kind_of("summary")["artifact_kind"], "report");
        assert_eq!(kind_of("definition")["artifact_kind"], "definition");
        let lifecycle = &kind_of("manual")["metadata"]["knowledge"]["lifecycle"];
        assert_eq!(lifecycle["revision"], 3);
        assert_eq!(lifecycle["status"], "resolved");
        assert_eq!(lifecycle["created_at"], "2026-10-01T00:00:00+00:00");
        assert_eq!(lifecycle["updated_at"], "2026-10-02T00:00:00+00:00");
    }

    #[test]
    fn convert_retired_kinds_skips_inactive_owner_and_continues_batch() {
        let mut inactive = row("inactive", "manual_note");
        inactive["semantic_element_id"] = Value::String("inactive-owner".into());
        let mut active = row("active", "derived_summary");
        active["semantic_element_id"] = Value::String("active-owner".into());
        let mut store = RawStore {
            rows: vec![inactive, active],
            inactive_owner_ids: HashSet::from(["inactive-owner".to_owned()]),
            active_owner_lookups: 0,
        };
        let rewritten = {
            let mut context = PluginContext::for_test(&mut store);
            convert_retired_kinds(&mut context, 1).expect("active row converts")
        };

        assert_eq!(rewritten, 1);
        assert_eq!(store.active_owner_lookups, 1);
        assert_eq!(store.rows[0]["artifact_kind"], "manual_note");
        assert_eq!(store.rows[1]["artifact_kind"], "report");
        assert_eq!(
            store.rows[0]["metadata"]["knowledge"]["lifecycle"]["revision"],
            3
        );
        assert_eq!(
            store.rows[1]["metadata"]["knowledge"]["lifecycle"]["status"],
            "resolved"
        );
    }
}
