use super::{
    Request, Snapshot,
    graph::{self, Filter},
    invalid, page,
};
use crate::require_non_empty;
use lumvise_contracts::SemanticElementV2;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn snippet(
    request: &Request,
    snapshot: &Snapshot,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let element = snapshot
        .elements
        .iter()
        .find(|element| element.semantic_element_id == request.semantic_element_id)
        .ok_or_else(|| {
            invalid(
                &request.semantic_element_id,
                "active source element in project",
            )
        })?;
    let start = element
        .start_line
        .filter(|line| *line > 0)
        .ok_or_else(|| invalid(&element.path, "source line range"))? as usize;
    let end = element
        .end_line
        .filter(|line| *line >= start as i64)
        .ok_or_else(|| invalid(&element.path, "source line range"))? as usize;
    let mut result = context.host_call("project.source", json!({"operation":"read","project_root":request.project_root,
        "path":element.path,"start_line":start.saturating_sub(request.context_lines).max(1),"end_line":end.saturating_add(request.context_lines)}))?;
    let indexed = snapshot
        .elements
        .iter()
        .find(|file| file.path == element.path && file.element_kind == "file")
        .and_then(|file| file.content_fingerprint.as_deref());
    result["index_matches_source"] = json!(
        indexed
            .zip(result["source_fingerprint"].as_str())
            .map(|(left, right)| left == right)
    );
    result["element"] = json!(element);
    result["source_revision"] = json!("current_worktree; ranges from indexed snapshot");
    Ok(result)
}

#[derive(Deserialize)]
struct SourceMatch {
    path: String,
    line: i64,
    text: String,
}
#[derive(Deserialize)]
struct SourceMatches {
    matches: Vec<SourceMatch>,
    failures: Vec<Value>,
}

pub(super) fn search(
    request: &Request,
    snapshot: &Snapshot,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    require_non_empty(&request.query, "query")?;
    let filter = Filter::new(request)?;
    let binary_paths: BTreeSet<_> = snapshot
        .elements
        .iter()
        .filter(|element| element.element_kind == "file")
        .filter(|file| matches!(extraction_status(file), Some("binary" | "unsupported")))
        .map(|file| file.path.as_str())
        .collect();
    let paths: BTreeSet<_> = snapshot
        .elements
        .iter()
        .filter(|element| {
            filter.accepts(element)
                && element.element_kind != "folder"
                && !binary_paths.contains(element.path.as_str())
        })
        .map(|element| &element.path)
        .collect();
    let output = context.host_call("project.source",json!({"operation":"search","project_root":request.project_root,
        "paths":paths,"pattern":request.query,"regex":request.regex,"case_sensitive":request.case_sensitive}))?;
    let found: SourceMatches = crate::parse(output, "source matches")?;
    let failure_count = found.failures.len();
    let by_path = elements_by_path(snapshot);
    let mut rows: Vec<_> = found
        .matches
        .into_iter()
        .filter_map(|found| ranked_match(found, request, &filter, &by_path))
        .collect();
    rows.sort_by(|a, b| {
        b["score"]
            .as_u64()
            .cmp(&a["score"].as_u64())
            .then(a["path"].as_str().cmp(&b["path"].as_str()))
            .then(a["line"].as_i64().cmp(&b["line"].as_i64()))
    });
    let mut response = page(rows, request);
    response["failures"] = json!(found.failures);
    response["failure_count"] = json!(failure_count);
    response["source_revision"] = json!("current_worktree; owner ranges from indexed snapshot");
    Ok(response)
}

fn extraction_status(file: &SemanticElementV2) -> Option<&str> {
    file.metadata
        .as_ref()?
        .get("indexer_metadata")?
        .get("extraction")?
        .get("status")?
        .as_str()
}

fn elements_by_path(snapshot: &Snapshot) -> BTreeMap<&str, Vec<&SemanticElementV2>> {
    let mut paths = BTreeMap::<&str, Vec<&SemanticElementV2>>::new();
    for element in &snapshot.elements {
        paths.entry(&element.path).or_default().push(element);
    }
    paths
}

fn ranked_match(
    found: SourceMatch,
    request: &Request,
    filter: &Filter,
    paths: &BTreeMap<&str, Vec<&SemanticElementV2>>,
) -> Option<Value> {
    let owner = paths
        .get(found.path.as_str())
        .into_iter()
        .flatten()
        .copied()
        .filter(|element| {
            element.start_line.is_some_and(|start| start <= found.line)
                && element.end_line.is_some_and(|end| end >= found.line)
        })
        .min_by_key(|element| {
            (
                element.end_line.unwrap_or(i64::MAX) - element.start_line.unwrap_or(0),
                element.element_kind == "file",
            )
        });
    if owner.is_some_and(|element| !filter.accepts(element)) {
        return None;
    }
    let score = 10
        + owner.map_or(0, |element| {
            graph::rank(element, &request.query.to_lowercase())
        });
    Some(
        json!({"path":found.path,"line":found.line,"text":found.text,"element":owner,"score":score}),
    )
}

pub(super) fn impact(
    request: &Request,
    snapshot: &Snapshot,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let changes = context.host_call(
        "project.source",
        json!({"operation":"git_changes","project_root":request.project_root,"base":request.base}),
    )?;
    let paths: BTreeSet<_> = changes["changes"]
        .as_array()
        .ok_or_else(|| invalid(&changes.to_string(), "git changes array"))?
        .iter()
        .filter_map(|change| change["path"].as_str())
        .collect();
    let roots: Vec<_> = snapshot
        .elements
        .iter()
        .filter(|element| paths.contains(element.path.as_str()))
        .map(|element| element.semantic_element_id.clone())
        .collect();
    let indexed: BTreeSet<_> = snapshot
        .elements
        .iter()
        .map(|element| element.path.as_str())
        .collect();
    let mut inbound = Request {
        direction: "inbound".into(),
        max_depth: request.max_depth,
        relationship_labels: request.relationship_labels.clone(),
        ..Request::default()
    };
    inbound.path_pattern = request.path_pattern.clone();
    let mut result = graph::trace(&inbound, snapshot, &roots)?;
    result["git"] = changes.clone();
    result["changed_element_ids"] = json!(roots);
    result["unindexed_paths"] = json!(paths.difference(&indexed).collect::<Vec<_>>());
    result["granularity"] = json!("file; includes every indexed symbol in each changed file");
    Ok(result)
}
