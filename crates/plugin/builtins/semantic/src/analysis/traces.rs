use super::{Request, Snapshot, invalid};
use crate::{require_non_empty, storage};
use lumvise_contracts::SemanticRelationshipV2;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const TABLE: &str = "code_runtime_traces";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedCall {
    source_element_id: String,
    target_element_id: String,
    #[serde(default = "one")]
    count: u64,
}
fn one() -> u64 {
    1
}

#[derive(Deserialize, Serialize)]
struct RuntimeTrace {
    trace_id: String,
    project_root: String,
    observed_at: String,
    edges: Vec<ObservedCall>,
}

pub(super) fn ingest(
    request: &Request,
    snapshot: &Snapshot,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    require_non_empty(&request.trace_id, "trace_id")?;
    let ids: BTreeSet<_> = snapshot
        .elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect();
    let mut counts = BTreeMap::<(&str, &str), u64>::new();
    for edge in &request.edges {
        if edge.count == 0
            || !ids.contains(edge.source_element_id.as_str())
            || !ids.contains(edge.target_element_id.as_str())
        {
            return Err(invalid(
                &format!(
                    "{} -> {} ({})",
                    edge.source_element_id, edge.target_element_id, edge.count
                ),
                "active project element IDs and positive count",
            ));
        }
        let count = counts
            .entry((&edge.source_element_id, &edge.target_element_id))
            .or_default();
        *count = count
            .checked_add(edge.count)
            .ok_or_else(|| invalid("call count overflow", "u64 total"))?;
    }
    let trace = RuntimeTrace {
        trace_id: request.trace_id.clone(),
        project_root: request.project_root.clone(),
        observed_at: chrono::Utc::now().to_rfc3339(),
        edges: counts
            .into_iter()
            .map(|((source, target), count)| ObservedCall {
                source_element_id: source.into(),
                target_element_id: target.into(),
                count,
            })
            .collect(),
    };
    let key = storage::project_key(&request.project_root, "runtime_trace", &[&request.trace_id]);
    storage::put(context, TABLE, &key, &trace)?;
    Ok(
        json!({"accepted":true,"trace_id":request.trace_id,"edges":trace.edges.len(),"mode":"replace_trace","static_index_unchanged":true}),
    )
}

pub(super) fn extend(
    snapshot: &mut Snapshot,
    context: &mut PluginContext<'_>,
) -> Result<(), PluginError> {
    let prefix = storage::project_prefix(&snapshot.project_root, "runtime_trace");
    let traces: Vec<RuntimeTrace> = storage::list(context, TABLE, Some(&prefix))?;
    let ids: BTreeSet<_> = snapshot
        .elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect();
    for trace in traces {
        for edge in trace.edges {
            if !ids.contains(edge.source_element_id.as_str())
                || !ids.contains(edge.target_element_id.as_str())
            {
                continue;
            }
            snapshot.relationships.push(SemanticRelationshipV2 {
                source_element_id:edge.source_element_id,target_element_id:edge.target_element_id,
                relationship_kind:"runtime".into(),label:"calls".into(),target_label:None,target_locator:None,
                project_root:Some(snapshot.project_root.clone()),lifecycle:Some("active".into()),
                metadata:Some(json!({"origin":"runtime","trace_id":trace.trace_id,"count":edge.count,"observed_at":trace.observed_at})) });
        }
    }
    Ok(())
}
