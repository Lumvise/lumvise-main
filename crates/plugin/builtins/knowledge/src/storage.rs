use std::collections::{BTreeMap, HashSet};

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::KnowledgeArtifact;
mod artifact_routes;
mod revision_reads;
pub(crate) use revision_reads::{artifact_at_edit, observe_knowledge, read_revision};

#[cfg(test)]
pub(crate) mod test_rows;

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

/// Atomically applies SDK mutations only while the observed row is unchanged.
/// Example: `mutate_rows_if_unchanged(context, table, key, Some(&pending), mutations)`.
pub(crate) fn mutate_rows_if_unchanged<T: Serialize>(
    context: &mut PluginContext<'_>,
    table: &str,
    key: &str,
    expected: Option<&T>,
    mutations: Vec<Value>,
) -> Result<Value, PluginError> {
    ensure_mutation_tables(context, table, &mutations)?;
    let expected = expected.map_or(
        json!({"presence":"absent"}),
        |value| json!({"presence":"present", "value":value}),
    );
    let mut conditional = vec![json!({"operation":"require_value", "table_name":table,
        "row_key":key, "expected":expected})];
    conditional.extend(mutations);
    context.host_call(
        CAPABILITY,
        json!({"operation":"mutate_rows", "mutations":conditional}),
    )
}

fn ensure_mutation_tables(
    context: &mut PluginContext<'_>,
    table: &str,
    mutations: &[Value],
) -> Result<(), PluginError> {
    let tables = std::iter::once(table)
        .chain(
            mutations
                .iter()
                .filter_map(|mutation| mutation["table_name"].as_str()),
        )
        .collect::<std::collections::BTreeSet<_>>();
    for table in tables {
        ensure(context, table)?;
    }
    Ok(())
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
    let mut artifacts = decode_graph_knowledge_artifacts(artifacts, project_root)?;
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

pub(crate) fn get_knowledge(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<Option<KnowledgeArtifact>, PluginError> {
    get_graph_artifact(context, artifact_id)
}

pub(crate) fn artifacts_for_elements(
    context: &mut PluginContext<'_>,
    semantic_element_ids: &HashSet<String>,
    project_root: Option<&str>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let rows = artifact_rows_for_elements(context, semantic_element_ids)?;
    match project_root {
        Some(root) => artifact_routes::project_rows(context, root, rows),
        None => artifact_routes::routed_rows(context, &rows, BTreeMap::new()),
    }
}

pub(crate) fn artifacts_for_sources(
    context: &mut PluginContext<'_>,
    elements: &[crate::semantic_context::SemanticElement],
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let ids = elements
        .iter()
        .map(|element| element.semantic_element_id.clone())
        .collect();
    let rows = artifact_rows_for_elements(context, &ids)?;
    let routes = elements
        .iter()
        .map(|element| {
            (
                element.semantic_element_id.clone(),
                element.project_root.clone(),
            )
        })
        .collect();
    artifact_routes::routed_rows(context, &rows, routes)
}

fn artifact_rows_for_elements(
    context: &mut PluginContext<'_>,
    ids: &HashSet<String>,
) -> Result<Vec<Value>, PluginError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation":"artifacts_for_elements","semantic_element_ids":ids}),
    )?;
    artifact_rows(&output).cloned()
}

fn artifact_rows(output: &Value) -> Result<&Vec<Value>, PluginError> {
    output
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("semantic Knowledge artifacts", output))
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
    artifact_routes::routed_rows(context, artifact_rows(&output)?, BTreeMap::new())
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

pub(crate) fn graph_artifact(value: &KnowledgeArtifact) -> Result<Value, PluginError> {
    let knowledge = serde_json::to_value(value).map_err(encode_error)?;
    let lifecycle = serde_json::to_value(&value.lifecycle).map_err(encode_error)?;
    Ok(json!({
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
    }))
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
    let Some(artifact) = raw_graph_artifact(context, artifact_id)?.filter(is_knowledge_artifact)
    else {
        return Ok(None);
    };
    let artifact = revision_reads::current_knowledge_body(context, artifact)?;
    decode_graph_artifact(&artifact).map(Some)
}

pub(crate) fn validate_knowledge_owner(
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

pub(crate) fn raw_graph_artifact(
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

pub(crate) fn decode_graph_knowledge_artifacts(
    artifacts: &[Value],
    project_root: &str,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    artifacts
        .iter()
        .filter(|artifact| is_knowledge_artifact(artifact))
        .map(|row| {
            let mut artifact: KnowledgeArtifact = decode_graph_artifact(row)?;
            artifact.project_root = Some(project_root.into());
            Ok(artifact)
        })
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

pub(crate) fn decode_graph_artifact<T: DeserializeOwned>(
    artifact: &Value,
) -> Result<T, PluginError> {
    let kind = artifact["artifact_kind"].as_str().unwrap_or_default();
    if !matches!(kind, "requirements" | "specification" | "media") {
        let root = artifact["metadata"]["knowledge"]["project_root"]
            .as_str()
            .or_else(|| artifact["project_root"].as_str())
            .unwrap_or("the owning project");
        return Err(PluginError::new(
            "unsupported_knowledge_format",
            format!(
                "Artifact `{}` has retired kind `{kind}`; expected requirements, specification or media. Project `{root}` needs a database in the current format; back up and rebuild with a compatible build.",
                artifact["artifact_id"]
            ),
            false,
        ));
    }
    let knowledge = &artifact["metadata"]["knowledge"];
    let mut record = json!({
        "edit_version":artifact["edit_version"],
        "artifact_id": artifact["artifact_id"],
        "semantic_element_id": artifact["semantic_element_id"],
        "knowledge_type": kind,
        "title": artifact["title"],
        "content": artifact["content"],
        "tags": knowledge["tags"],
        "metadata": knowledge["metadata"],
        "dependencies": artifact["dependencies"],
        "path": knowledge["path"],
        "project_root": artifact.get("project_root").unwrap_or(&knowledge["project_root"])
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

    fn row(kind: &str) -> Value {
        json!({"project_root":"/repo","artifact_id":"a","semantic_element_id":"file:src/a.rs",
            "artifact_kind":kind,"title":"Title","content":"Body","dependencies":[],
            "metadata":{"knowledge":{"tags":[],"metadata":{},"path":null,"project_root":"/repo",
            "lifecycle":{"status":"resolved","revision":3}}}})
    }

    #[test]
    fn current_kinds_decode_without_reclassifying_the_stored_record() {
        for kind in ["requirements", "specification", "media"] {
            let artifact: KnowledgeArtifact = decode_graph_artifact(&row(kind)).unwrap();
            assert_eq!(serde_json::to_value(artifact.knowledge_type).unwrap(), kind);
            assert_eq!(artifact.lifecycle.revision, 3);
            assert_eq!(
                artifact.lifecycle.status,
                lumvise_contracts::KnowledgeArtifactStatusV2::Resolved
            );
        }
    }

    #[test]
    fn retired_kinds_are_rejected_without_rewriting_their_bodies() {
        for kind in [
            "issue",
            "task_assignment",
            "definition",
            "annotation",
            "report",
            "decision",
            "checkpoint",
            "question",
            "plan",
            "manual_note",
            "derived_summary",
        ] {
            let error = decode_graph_artifact::<KnowledgeArtifact>(&row(kind)).unwrap_err();
            assert_eq!(error.code, "unsupported_knowledge_format");
            assert!(error.message.contains(kind));
            assert!(error.message.contains("back up and rebuild"));
        }
    }
}
