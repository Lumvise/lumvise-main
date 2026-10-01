use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const ELEMENTS: &str = "semantic_elements";
pub(crate) const SOURCES: &str = "semantic_sources";
pub(crate) const SEARCH_INDEX_STATE: &str = "semantic_search_index_state";
pub(crate) const INDEX_LOGS: &str = "semantic_index_logs";
pub(crate) const INDEX_LOGS_BY_PROVIDER: &str = "semantic_index_logs_by_provider";

const CAPABILITY: &str = "storage.plugin";
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ProjectSnapshot<E, R, A> {
    pub commit_version: i64,
    pub published_at: String,
    pub project_root: String,
    pub elements: Vec<E>,
    pub relationships: Vec<R>,
    pub artifacts: Vec<A>,
    #[serde(default)]
    pub scope_element_count: Option<usize>,
}

pub(crate) fn project_snapshot<E: DeserializeOwned, R: DeserializeOwned, A: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    scope: &Value,
    artifact_namespace: Option<&str>,
) -> Result<ProjectSnapshot<E, R, A>, PluginError> {
    let output = context
        .host_call(
            SEMANTIC_CAPABILITY,
            json!({"operation": "project_snapshot", "scope": scope,
                "artifact_namespace": artifact_namespace}),
        )
        .map_err(|error| map_snapshot_scope_error(error, scope))?;
    serde_json::from_value(output).map_err(|error| {
        PluginError::new(
            "invalid_semantic_storage",
            format!("invalid project snapshot response: {error}"),
            false,
        )
    })
}

pub(crate) fn structure_snapshot<E: DeserializeOwned, R: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    scope: &Value,
    max_depth: usize,
    include_inactive: bool,
) -> Result<ProjectSnapshot<E, R, Value>, PluginError> {
    scoped_snapshot(
        context,
        json!({"Structure": {
            "scope": scope, "max_depth": max_depth, "include_inactive": include_inactive
        }}),
    )
    .map_err(|error| map_snapshot_scope_error(error, scope))
}

pub(crate) fn location_snapshot<E: DeserializeOwned, R: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    project_root: &str,
    path: Option<&str>,
    line: i64,
    include_inactive: bool,
) -> Result<ProjectSnapshot<E, R, Value>, PluginError> {
    scoped_snapshot(
        context,
        json!({"Location": {
            "project_root": project_root, "path": path, "line": line, "include_inactive": include_inactive
        }}),
    )
}

pub(crate) fn dependency_snapshot<E: DeserializeOwned, R: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    semantic_element_id: &str,
    direction: &str,
    max_depth: usize,
    include_descendants: bool,
    include_inactive: bool,
) -> Result<ProjectSnapshot<E, R, Value>, PluginError> {
    scoped_snapshot(context, json!({"Dependency": {
        "semantic_element_id": semantic_element_id, "direction": direction,
        "max_depth": max_depth, "include_descendants": include_descendants, "include_inactive": include_inactive
    }})).map_err(|error| map_snapshot_scope_error(error, &json!({"semantic_element": semantic_element_id})))
}

fn scoped_snapshot<E: DeserializeOwned, R: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    request: Value,
) -> Result<ProjectSnapshot<E, R, Value>, PluginError> {
    let mut output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "scoped_read", "request": request}),
    )?;
    output["artifacts"] = json!([]);
    serde_json::from_value(output).map_err(|error| {
        PluginError::new(
            "invalid_semantic_storage",
            format!("invalid scoped graph response: {error}"),
            false,
        )
    })
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RendererGraphProjection {
    #[serde(rename = "commitVersion")]
    pub commit_version: i64,
    #[serde(rename = "publishedAt")]
    pub published_at: String,
    pub nodes: Vec<Value>,
    pub edges: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RendererGraphRequest {
    pub project_root: String,
    pub target_path: Option<String>,
    pub granularity: crate::models::GraphGranularity,
    pub recursive: bool,
    pub include_external: bool,
    pub include_first_neighbors: bool,
}

pub(crate) fn project_renderer_graph(
    context: &mut PluginContext<'_>,
    request: RendererGraphRequest,
) -> Result<RendererGraphProjection, PluginError> {
    let mut input = serde_json::to_value(request).map_err(|error| {
        PluginError::new(
            "invalid_semantic_storage",
            format!("invalid renderer graph projection request: {error}"),
            false,
        )
    })?;
    input["operation"] = Value::String("project_renderer_graph".into());
    let output = context
        .host_call(SEMANTIC_CAPABILITY, input)
        .map_err(|error| {
            PluginError::new(
                error.code,
                format!("renderer graph projection failed: {}", error.message),
                error.retryable,
            )
        })?;
    serde_json::from_value(output).map_err(|error| {
        PluginError::new(
            "invalid_semantic_storage",
            format!("invalid renderer graph projection response: {error}"),
            false,
        )
    })
}

fn map_snapshot_scope_error(error: PluginError, scope: &Value) -> PluginError {
    let Some(element_id) = scope.get("semantic_element").and_then(Value::as_str) else {
        return error;
    };
    if !error.message.contains("expected existing semantic element") {
        return error;
    }
    PluginError::new(
        "semantic_element_not_found",
        format!("semantic element `{element_id}` is missing; expected indexed semantic element"),
        false,
    )
}

const SEMANTIC_CAPABILITY: &str = "storage.semantic";
const PAGE_LIMIT: usize = 500;
const MUTATION_PAGE_LIMIT: usize = 500;
const MUTATION_PAGE_BYTES: usize = 1024 * 1024;

pub(crate) fn sync_structure<T: Serialize, R: Serialize>(
    context: &mut PluginContext<'_>,
    project_root: &str,
    elements: &[T],
    relationships: &[R],
) -> Result<Value, PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "sync_structure", "project_root": project_root,
            "elements": elements, "relationships": relationships}),
    )
}

pub(crate) fn begin_project_snapshot(
    context: &mut PluginContext<'_>,
    snapshot_id: &str,
    project_root: &str,
    partition_paths: &[String],
    page_count: usize,
) -> Result<(), PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "begin_project_snapshot", "snapshot_id": snapshot_id,
            "project_root": project_root, "partition_paths": partition_paths,
            "page_count": page_count}),
    )?;
    Ok(())
}

pub(crate) fn stage_project_snapshot<T: Serialize, R: Serialize>(
    context: &mut PluginContext<'_>,
    snapshot_id: &str,
    page_index: usize,
    elements: &[T],
    relationships: &[R],
) -> Result<(), PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "stage_project_snapshot", "snapshot_id": snapshot_id,
            "page_index": page_index, "elements": elements, "relationships": relationships}),
    )?;
    Ok(())
}

pub(crate) fn commit_project_snapshot(
    context: &mut PluginContext<'_>,
    snapshot_id: &str,
) -> Result<Value, PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "commit_project_snapshot", "snapshot_id": snapshot_id}),
    )
}

pub(crate) fn sync_partition<T: Serialize, R: Serialize>(
    context: &mut PluginContext<'_>,
    project_root: &str,
    replace_paths: &[String],
    elements: &[T],
    relationships: &[R],
) -> Result<Value, PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "sync_partition", "partition": {
            "project_root": project_root, "replace_paths": replace_paths},
            "elements": elements, "relationships": relationships}),
    )
}

pub(crate) fn upsert_graph_artifact<T: Serialize>(
    context: &mut PluginContext<'_>,
    artifact: &T,
) -> Result<(), PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "upsert_artifact", "artifact": artifact}),
    )?;
    Ok(())
}

pub(crate) fn remove_graph_artifact(
    context: &mut PluginContext<'_>,
    artifact_id: &str,
) -> Result<bool, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "remove_artifact", "artifact_id": artifact_id}),
    )?;
    output["removed"].as_bool().ok_or_else(|| {
        PluginError::new(
            "invalid_semantic_storage_response",
            format!(
                "invalid semantic artifact removal response `{output}`; expected removed boolean"
            ),
            false,
        )
    })
}
pub(crate) fn graph_elements_by_ids<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    project_root: &str,
    semantic_element_ids: &[String],
) -> Result<Vec<T>, PluginError> {
    semantic_array(
        context,
        json!({"operation": "elements_by_ids", "project_root": project_root,
            "semantic_element_ids": semantic_element_ids}),
        "elements",
    )
}

pub(crate) fn graph_artifacts_by_ids<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    artifact_ids: &[String],
) -> Result<Vec<T>, PluginError> {
    semantic_array(
        context,
        json!({"operation": "artifacts_by_ids", "artifact_ids": artifact_ids}),
        "artifacts",
    )
}

pub(crate) fn graph_search_element_candidates<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    project_root: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<T>, PluginError> {
    semantic_array(
        context,
        json!({"operation": "search_element_candidates", "project_root": project_root,
            "query": query, "limit": limit}),
        "elements",
    )
}

pub(crate) fn graph_project_roots(
    context: &mut PluginContext<'_>,
) -> Result<Vec<String>, PluginError> {
    semantic_array(
        context,
        json!({"operation": "project_roots"}),
        "project_roots",
    )
}

pub(crate) fn graph_element<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    semantic_element_id: &str,
) -> Result<Option<T>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "element", "semantic_element_id": semantic_element_id}),
    )?;
    output
        .get("element")
        .filter(|element| !element.is_null())
        .map(|element| decode(element, ELEMENTS))
        .transpose()
}

pub(crate) fn store_element_name_vectors(
    context: &mut PluginContext<'_>,
    project_root: &str,
    vectors: Vec<Value>,
) -> Result<usize, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "store_element_name_vectors", "project_root": project_root,
            "vectors": vectors}),
    )?;
    Ok(output.get("stored").and_then(Value::as_u64).unwrap_or(0) as usize)
}

pub(crate) fn store_artifact_text_vectors(
    context: &mut PluginContext<'_>,
    project_root: &str,
    vectors: Vec<Value>,
) -> Result<usize, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation": "store_artifact_text_vectors", "project_root": project_root,
            "vectors": vectors}),
    )?;
    Ok(output.get("stored").and_then(Value::as_u64).unwrap_or(0) as usize)
}

fn semantic_array<T: DeserializeOwned>(
    context: &mut PluginContext<'_>,
    input: Value,
    field: &str,
) -> Result<Vec<T>, PluginError> {
    let output = context.host_call(SEMANTIC_CAPABILITY, input)?;
    let records = output.get(field).and_then(Value::as_array).ok_or_else(|| {
        PluginError::new(
            "invalid_semantic_storage",
            format!("invalid storage.semantic response `{output}`; expected array field `{field}`"),
            false,
        )
    })?;
    records.iter().map(|record| decode(record, field)).collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum Mutation {
    Put {
        table_name: String,
        row_key: String,
        value: Value,
    },
}

pub(crate) fn put_mutation<T: Serialize>(
    table: &str,
    row_key: &str,
    value: &T,
) -> Result<Mutation, PluginError> {
    Ok(Mutation::Put {
        table_name: table.into(),
        row_key: row_key.into(),
        value: serde_json::to_value(value).map_err(|error| {
            PluginError::new(
                "invalid_semantic_storage",
                format!("failed to encode row `{row_key}` for table `{table}`: {error}"),
                false,
            )
        })?,
    })
}

pub(crate) fn apply_mutations(
    context: &mut PluginContext<'_>,
    mutations: &[Mutation],
) -> Result<(), PluginError> {
    if mutations.is_empty() {
        return Ok(());
    }
    ensure_mutation_tables(context, mutations)?;
    let begin = context.host_call(CAPABILITY, json!({"operation": "begin_mutations"}))?;
    let transaction_id = begin
        .get("transaction_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_response("transaction", &begin))?
        .to_owned();
    let mut offset = 0;
    while offset < mutations.len() {
        let end = mutation_page_end(mutations, offset)?;
        let chunk = &mutations[offset..end];
        if let Err(error) = stage_mutations(context, &transaction_id, chunk) {
            abort_mutations(context, &transaction_id);
            return Err(error);
        }
        offset = end;
    }
    context.host_call(
        CAPABILITY,
        json!({"operation": "commit_mutations", "transaction_id": transaction_id}),
    )?;
    Ok(())
}

fn mutation_page_end(mutations: &[Mutation], offset: usize) -> Result<usize, PluginError> {
    let upper = (offset + MUTATION_PAGE_LIMIT).min(mutations.len());
    if mutation_page_bytes(&mutations[offset..upper])? <= MUTATION_PAGE_BYTES {
        return Ok(upper);
    }
    let mut fitting = offset + 1;
    let mut rejected = upper;
    while fitting + 1 < rejected {
        let candidate = fitting + (rejected - fitting) / 2;
        if mutation_page_bytes(&mutations[offset..candidate])? <= MUTATION_PAGE_BYTES {
            fitting = candidate;
        } else {
            rejected = candidate;
        }
    }
    Ok(fitting)
}

fn mutation_page_bytes(mutations: &[Mutation]) -> Result<usize, PluginError> {
    serde_json::to_vec(mutations)
        .map(|bytes| bytes.len())
        .map_err(|error| {
            PluginError::new(
                "invalid_semantic_storage",
                format!("failed to encode Semantic mutation page: {error}"),
                false,
            )
        })
}

fn ensure_mutation_tables(
    context: &mut PluginContext<'_>,
    mutations: &[Mutation],
) -> Result<(), PluginError> {
    let mut tables = mutations.iter().map(mutation_table).collect::<Vec<_>>();
    tables.sort_unstable();
    tables.dedup();
    for table in tables {
        ensure(context, table)?;
    }
    Ok(())
}

fn mutation_table(mutation: &Mutation) -> &str {
    match mutation {
        Mutation::Put { table_name, .. } => table_name,
    }
}

fn stage_mutations(
    context: &mut PluginContext<'_>,
    transaction_id: &str,
    mutations: &[Mutation],
) -> Result<(), PluginError> {
    context.host_call(
        CAPABILITY,
        json!({
            "operation": "stage_mutations",
            "transaction_id": transaction_id,
            "mutations": mutations
        }),
    )?;
    Ok(())
}

fn abort_mutations(context: &mut PluginContext<'_>, transaction_id: &str) {
    let _ = context.host_call(
        CAPABILITY,
        json!({"operation": "abort_mutations", "transaction_id": transaction_id}),
    );
}

pub(crate) fn put<T: Serialize>(
    context: &mut PluginContext<'_>,
    table: &str,
    row_key: &str,
    value: &T,
) -> Result<(), PluginError> {
    ensure(context, table)?;
    context.host_call(
        CAPABILITY,
        json!({
            "operation": "put_row", "table_name": table, "row_key": row_key, "value": value
        }),
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
        json!({
            "operation": "get_row", "table_name": table, "row_key": row_key
        }),
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
    ensure(context, table)?;
    let mut records = Vec::new();
    let mut after_key: Option<String> = None;
    loop {
        let mut input = json!({
            "operation": "list_rows", "table_name": table, "limit": PAGE_LIMIT
        });
        if let Some(prefix) = key_prefix {
            input["key_prefix"] = json!(prefix);
        }
        if let Some(cursor) = after_key.as_deref() {
            input["after_key"] = json!(cursor);
        }
        let output = context.host_call(CAPABILITY, input)?;
        let rows = output
            .get("rows")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_response(table, &output))?;
        records.extend(
            rows.iter()
                .map(|row| decode(row.get("value").unwrap_or(&Value::Null), table))
                .collect::<Result<Vec<T>, _>>()?,
        );
        let next_after_key = output
            .get("next_after_key")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if next_after_key.is_some() && next_after_key == after_key {
            return Err(PluginError::new(
                "invalid_storage_response",
                format!(
                    "storage.plugin repeated cursor `{next_after_key:?}` for table `{table}`; expected advancing next_after_key"
                ),
                false,
            ));
        }
        after_key = next_after_key;
        if after_key.is_none() {
            return Ok(records);
        }
    }
}

pub(crate) fn project_prefix(project_root: &str, entity: &str) -> String {
    scoped_prefix("p", project_root, entity)
}

pub(crate) fn project_key(project_root: &str, entity: &str, parts: &[&str]) -> String {
    format!(
        "{}{}:{}",
        project_prefix(project_root, entity),
        encoded_identity(parts),
        digest(parts)
    )
}

pub(crate) fn global_key(entity: &str, parts: &[&str]) -> String {
    format!("g:{entity}:{}:{}", encoded_identity(parts), digest(parts))
}

pub(crate) fn scoped_prefix(scope: &str, value: &str, entity: &str) -> String {
    format!("{scope}:{}:{entity}:", hex_encode(value))
}

pub(crate) fn scoped_key(scope: &str, value: &str, entity: &str, parts: &[&str]) -> String {
    format!(
        "{}{}:{}",
        scoped_prefix(scope, value, entity),
        encoded_identity(parts),
        digest(parts)
    )
}

fn encoded_identity(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| hex_encode(part))
        .collect::<Vec<_>>()
        .join(".")
}

fn digest(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update([0]);
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn hex_encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ensure(context: &mut PluginContext<'_>, table: &str) -> Result<(), PluginError> {
    context.host_call(
        CAPABILITY,
        json!({
            "operation": "ensure_table", "table_name": table,
            "schema": {"type": "object"}
        }),
    )?;
    Ok(())
}

fn decode<T: DeserializeOwned>(value: &Value, table: &str) -> Result<T, PluginError> {
    serde_json::from_value(value.clone()).map_err(|error| PluginError::new(
        "invalid_semantic_storage",
        format!("invalid stored value `{value}` in table `{table}`; expected Semantic record: {error}"),
        false,
    ))
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

pub(crate) fn project_element_counts(
    context: &mut PluginContext<'_>,
    project_root: &str,
) -> Result<Value, PluginError> {
    context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation":"project_element_counts","project_root":project_root}),
    )
}
