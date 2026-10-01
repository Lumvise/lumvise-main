use lumvise_contracts::{
    SemanticContextRequestV2, SemanticContextResponseV2, SemanticElementV2,
    SemanticIndexedArtifactV2, SemanticRelationshipV2,
};
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::{parse, require_non_empty, serialize_response, storage};

pub(crate) fn semantic_context(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: SemanticContextRequestV2 = parse(input, "semantic context request")?;
    require_non_empty(&request.project_root, "project_root")?;
    require_non_empty(&request.record_kind, "record_kind")?;
    if !matches!(
        request.record_kind.as_str(),
        "elements" | "relationships" | "artifacts"
    ) {
        return Err(invalid(
            &request.record_kind,
            "record_kind in elements|relationships|artifacts",
        ));
    }
    let snapshot = context_snapshot(context, &request)?;
    let artifacts = snapshot
        .artifacts
        .into_iter()
        .map(serde_json::from_value::<SemanticIndexedArtifactV2>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(&e.to_string(), "semantic artifacts"))?;
    let mut response = SemanticContextResponseV2 {
        record_kind: request.record_kind,
        elements: snapshot.elements,
        relationships: snapshot.relationships,
        artifacts,
        commit_version: snapshot.commit_version,
        published_at: snapshot.published_at,
    };
    restrict_context(
        &mut response,
        request.root_element_id.as_deref(),
        request.include_descendants.unwrap_or(true),
    )?;
    serialize_response(response)
}

// The scoped read retains full element records, including document selectors.
// Keep the complete snapshot fallback for trees reaching the storage depth limit:
// semantic_context promises every descendant, without truncation or mixed revisions.
fn context_snapshot(
    context: &mut PluginContext<'_>,
    request: &SemanticContextRequestV2,
) -> Result<storage::ProjectSnapshot<SemanticElementV2, SemanticRelationshipV2, Value>, PluginError>
{
    if let Some(root) = request
        .root_element_id
        .as_deref()
        .filter(|_| request.record_kind == "elements")
    {
        let depth = if request.include_descendants.unwrap_or(true) {
            128
        } else {
            0
        };
        let snapshot = storage::structure_snapshot::<SemanticElementV2, SemanticRelationshipV2>(
            context,
            &serde_json::json!({"semantic_element": root}),
            depth,
            true,
        )?;
        if snapshot.project_root != request.project_root {
            return Err(invalid(
                root,
                "root_element_id belonging to the requested project",
            ));
        }
        if depth == 0
            || !reaches_context_depth(&snapshot.elements, &snapshot.relationships, root, depth)
        {
            return Ok(snapshot);
        }
    }
    storage::project_snapshot(
        context,
        &serde_json::json!({"project_root": request.project_root}),
        None,
    )
}

fn reaches_context_depth(
    elements: &[SemanticElementV2],
    relationships: &[SemanticRelationshipV2],
    root: &str,
    max_depth: usize,
) -> bool {
    let children = context_children(elements, relationships);
    let mut pending = VecDeque::from([(root.to_owned(), 0)]);
    let mut visited = HashSet::new();
    while let Some((id, depth)) = pending.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        if depth == max_depth {
            return true;
        }
        if let Some(children) = children.get(&id) {
            pending.extend(children.iter().map(|child| (child.clone(), depth + 1)));
        }
    }
    false
}

fn restrict_context(
    response: &mut SemanticContextResponseV2,
    root: Option<&str>,
    include_descendants: bool,
) -> Result<(), PluginError> {
    if let Some(root) = root {
        let selected = context_element_ids(response, root, include_descendants)?;
        retain_context_elements(response, &selected);
    }
    if response.record_kind != "elements" {
        response.elements.clear();
    }
    if response.record_kind != "relationships" {
        response.relationships.clear();
    }
    if response.record_kind != "artifacts" {
        response.artifacts.clear();
    }
    Ok(())
}

fn retain_context_elements(response: &mut SemanticContextResponseV2, selected: &HashSet<String>) {
    response
        .elements
        .retain(|element| selected.contains(&element.semantic_element_id));
    response.relationships.retain(|relationship| {
        selected.contains(&relationship.source_element_id)
            || selected.contains(&relationship.target_element_id)
    });
    response
        .artifacts
        .retain(|artifact| selected.contains(&artifact.semantic_element_id));
}

fn context_element_ids(
    response: &SemanticContextResponseV2,
    root: &str,
    include_descendants: bool,
) -> Result<HashSet<String>, PluginError> {
    if !response
        .elements
        .iter()
        .any(|element| element.semantic_element_id == root)
    {
        return Err(invalid(
            root,
            "root_element_id belonging to the requested project",
        ));
    }
    let mut selected = HashSet::from([root.to_owned()]);
    if include_descendants {
        crate::query::collect_descendants(
            root,
            &context_children(&response.elements, &response.relationships),
            &mut selected,
        );
    }
    Ok(selected)
}

fn context_children(
    elements: &[SemanticElementV2],
    relationships: &[SemanticRelationshipV2],
) -> HashMap<String, Vec<String>> {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let parents = elements.iter().filter_map(|element| {
        element
            .parent_element_id
            .as_ref()
            .map(|parent| (parent, &element.semantic_element_id))
    });
    let containment = relationships
        .iter()
        .filter(|relationship| {
            relationship.relationship_kind == "contains" || relationship.label == "contains"
        })
        .map(|relationship| {
            (
                &relationship.source_element_id,
                &relationship.target_element_id,
            )
        });
    for (parent, child) in parents.chain(containment) {
        children
            .entry(parent.clone())
            .or_default()
            .push(child.clone());
    }
    children
}

fn invalid(value: &str, expected: &str) -> PluginError {
    PluginError::new(
        "invalid_semantic_input",
        format!("invalid Semantic context value `{value}`; expected {expected}"),
        false,
    )
}
