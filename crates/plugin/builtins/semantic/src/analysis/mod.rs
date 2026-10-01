//! Owns derived code intelligence over one coherent Semantic snapshot.
//! Manifest dispatch is the public entrypoint. Source I/O stays in App Core;
//! observations are a separate plugin-owned overlay, never a replacement static index.
mod graph;
pub(crate) mod manifest;
mod metrics;
mod source;
#[cfg(test)]
mod tests;
mod traces;

use crate::{parse, require_non_empty, storage};
use lumvise_contracts::{SemanticElementV2, SemanticRelationshipV2};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};

type Snapshot = storage::ProjectSnapshot<SemanticElementV2, SemanticRelationshipV2, Value>;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Request {
    project_root: String,
    query: String,
    name_pattern: Option<String>,
    path_pattern: Option<String>,
    element_kind: Option<String>,
    case_sensitive: bool,
    regex: bool,
    offset: usize,
    limit: usize,
    expected_commit_version: Option<i64>,
    semantic_element_id: String,
    context_lines: usize,
    direction: String,
    relationship_labels: Vec<String>,
    max_depth: usize,
    base: String,
    trace_id: String,
    edges: Vec<traces::ObservedCall>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            project_root: String::new(),
            query: String::new(),
            name_pattern: None,
            path_pattern: None,
            element_kind: None,
            case_sensitive: true,
            regex: false,
            offset: 0,
            limit: 50,
            expected_commit_version: None,
            semantic_element_id: String::new(),
            context_lines: 0,
            direction: "outbound".into(),
            relationship_labels: vec!["calls".into()],
            max_depth: 3,
            base: "HEAD".into(),
            trace_id: String::new(),
            edges: Vec::new(),
        }
    }
}

pub(crate) fn invoke(
    id: &str,
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: Request = parse(input, "code intelligence request")?;
    require_non_empty(&request.project_root, "project_root")?;
    if request.limit == 0 {
        return Err(invalid("0", "positive limit"));
    }
    let mut snapshot = active_snapshot(&request, context)?;
    if id == "ingest_runtime_trace" {
        return traces::ingest(&request, &snapshot, context);
    }
    if matches!(
        id,
        "trace_path" | "get_graph_schema" | "get_graph_metrics" | "get_git_impact"
    ) {
        traces::extend(&mut snapshot, context)?;
    }
    let mut response = dispatch(id, &request, &snapshot, context)?;
    response["commit_version"] = json!(snapshot.commit_version);
    response["published_at"] = json!(snapshot.published_at);
    Ok(response)
}

fn active_snapshot(
    request: &Request,
    context: &mut PluginContext<'_>,
) -> Result<Snapshot, PluginError> {
    let mut snapshot: Snapshot = storage::project_snapshot(
        context,
        &json!({"project_root":request.project_root}),
        Some("code_intelligence"),
    )?;
    if request
        .expected_commit_version
        .is_some_and(|version| version != snapshot.commit_version)
    {
        return Err(invalid(
            &format!("{:?}", request.expected_commit_version),
            "current commit_version; restart pagination after index changes",
        ));
    }
    snapshot.elements.retain(|element| {
        element
            .lifecycle
            .as_deref()
            .is_none_or(|value| value == "active")
    });
    snapshot.relationships.retain(|edge| {
        edge.lifecycle
            .as_deref()
            .is_none_or(|value| value == "active")
    });
    Ok(snapshot)
}

fn dispatch(
    id: &str,
    request: &Request,
    snapshot: &Snapshot,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    match id {
        "search_graph" => graph::search(request, snapshot),
        "trace_path" => graph::trace(
            request,
            snapshot,
            std::slice::from_ref(&request.semantic_element_id),
        ),
        "get_graph_schema" => Ok(metrics::schema(snapshot)),
        "get_graph_metrics" => metrics::metrics(request, snapshot),
        "check_index_coverage" => metrics::coverage(request, snapshot),
        "get_code_snippet" => source::snippet(request, snapshot, context),
        "search_code" => source::search(request, snapshot, context),
        "get_git_impact" => source::impact(request, snapshot, context),
        _ => Err(PluginError::unknown_capability(id)),
    }
}

fn invalid(value: &str, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_code_intelligence",
        format!("invalid `{value}`; expected {expected}"),
        false,
    )
}

fn page(mut rows: Vec<Value>, request: &Request) -> Value {
    let total = rows.len();
    let end = request.offset.saturating_add(request.limit).min(total);
    let matches: Vec<_> = rows.drain(request.offset.min(total)..end).collect();
    json!({"matches":matches,"total":total,"offset":request.offset,"next_offset":(end < total).then_some(end)})
}
