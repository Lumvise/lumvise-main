use lumvise_contracts::ArtifactDependency;
use std::collections::BTreeSet;

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SemanticElement {
    pub project_root: String,
    pub semantic_element_id: String,
    pub semantic_source_id: String,
    pub path: String,
    pub element_kind: String,
    pub name: String,
    pub parent_element_id: Option<String>,
    pub content_fingerprint: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub lifecycle: String,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SemanticRelationship {
    pub project_root: String,
    pub source_element_id: String,
    pub target_element_id: String,
    pub relationship_kind: String,
    pub label: String,
    pub lifecycle: String,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SemanticArtifact {
    pub project_root: String,
    pub artifact_id: String,
    pub semantic_element_id: String,
    pub artifact_kind: String,
    pub title: String,
    pub content_ref: Option<String>,
    pub content: Option<String>,
    pub searchable_text: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<ArtifactDependency>,
    pub content_size_bytes: Option<usize>,
    pub metadata: Value,
}

#[derive(Default)]
pub(crate) struct SemanticContext {
    pub elements: Vec<SemanticElement>,
    pub relationships: Vec<SemanticRelationship>,
    pub artifacts: Vec<SemanticArtifact>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SelectiveSubgraph {
    pub commit_version: i64,
    pub published_at: String,
    pub project_root: String,
    pub root_element_id: String,
    #[serde(default)]
    pub elements: Vec<SemanticElement>,
    #[serde(default)]
    pub relationships: Vec<SemanticRelationship>,
    #[serde(default)]
    pub artifacts: Vec<SemanticArtifact>,
    #[serde(default)]
    pub external_elements: Vec<SemanticElement>,
}

pub(crate) fn load_selective_subgraph(
    context: &mut PluginContext<'_>,
    project_root: &str,
    root_element_id: &str,
) -> Result<Option<SelectiveSubgraph>, PluginError> {
    let output = context.host_call(
        "storage.semantic",
        json!({
            "operation": "selective_subgraph",
            "project_root": project_root,
            "root_element_id": root_element_id,
            "artifact_namespace": "knowledge"
        }),
    )?;
    let value = output.get("subgraph").unwrap_or(&output);
    if value.is_null() {
        return Ok(None);
    }
    let mut subgraph: SelectiveSubgraph =
        serde_json::from_value(value.clone()).map_err(|error| {
            PluginError::new(
                "invalid_plugin_invoke_response",
                format!("invalid AppCore selective semantic subgraph `{value}`: {error}"),
                false,
            )
        })?;
    if subgraph.project_root != project_root || subgraph.root_element_id != root_element_id {
        return Err(PluginError::new(
            "invalid_plugin_invoke_response",
            format!(
                "selective semantic subgraph identity mismatch: requested `{project_root}`/`{root_element_id}`, got `{}`/`{}`",
                subgraph.project_root, subgraph.root_element_id
            ),
            false,
        ));
    }
    for element in subgraph
        .elements
        .iter()
        .chain(subgraph.external_elements.iter())
    {
        if element.project_root != project_root {
            return Err(PluginError::new(
                "invalid_plugin_invoke_response",
                "selective subgraph contains a cross-project element",
                false,
            ));
        }
    }
    for relationship in &subgraph.relationships {
        if relationship.project_root != project_root {
            return Err(PluginError::new(
                "invalid_plugin_invoke_response",
                "selective subgraph contains a cross-project relationship",
                false,
            ));
        }
    }
    for artifact in &subgraph.artifacts {
        if artifact.project_root != project_root {
            return Err(PluginError::new(
                "invalid_plugin_invoke_response",
                "selective subgraph contains a cross-project artifact",
                false,
            ));
        }
    }
    subgraph.elements.append(&mut subgraph.external_elements);
    subgraph
        .elements
        .sort_by(|left, right| left.semantic_element_id.cmp(&right.semantic_element_id));
    subgraph
        .elements
        .dedup_by(|left, right| left.semantic_element_id == right.semantic_element_id);
    subgraph.relationships.sort_by(|left, right| {
        left.source_element_id
            .cmp(&right.source_element_id)
            .then(left.target_element_id.cmp(&right.target_element_id))
            .then(left.relationship_kind.cmp(&right.relationship_kind))
            .then(left.label.cmp(&right.label))
    });
    subgraph
        .artifacts
        .sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    Ok(Some(subgraph))
}

pub(crate) struct ProjectProjectionSnapshot {
    pub commit_version: i64,
    pub published_at: String,
    pub semantic: SemanticContext,
    pub artifacts: Vec<crate::KnowledgeArtifact>,
}

#[derive(Deserialize)]
struct CompleteProjectSnapshot {
    commit_version: i64,
    published_at: String,
    project_root: String,
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
    artifacts: Vec<Value>,
}

pub(crate) fn current_revision(context: &mut PluginContext<'_>) -> Result<i64, PluginError> {
    let output = context.host_call(
        "storage.semantic",
        json!({"operation": "semantic_revision"}),
    )?;
    output["commit_version"].as_i64().ok_or_else(|| {
        PluginError::new(
            "invalid_plugin_invoke_response",
            format!(
                "invalid AppCore semantic revision `{output}`; expected commit_version integer"
            ),
            false,
        )
    })
}

pub(crate) fn load_projection_snapshot(
    context: &mut PluginContext<'_>,
    project_root: &str,
) -> Result<ProjectProjectionSnapshot, PluginError> {
    let snapshot = complete_project_snapshot(context, project_root, Some("knowledge"))?;
    Ok(ProjectProjectionSnapshot {
        commit_version: snapshot.commit_version,
        published_at: snapshot.published_at,
        semantic: SemanticContext {
            elements: snapshot.elements,
            relationships: snapshot.relationships,
            artifacts: Vec::new(),
        },
        artifacts: crate::storage::decode_graph_knowledge_artifacts(&snapshot.artifacts)?,
    })
}

fn complete_project_snapshot(
    context: &mut PluginContext<'_>,
    project_root: &str,
    artifact_namespace: Option<&str>,
) -> Result<CompleteProjectSnapshot, PluginError> {
    let mut input = json!({"operation": "project_snapshot",
        "scope": {"project_root": project_root}});
    if let Some(namespace) = artifact_namespace {
        input["artifact_namespace"] = json!(namespace);
    }
    let output = context.host_call("storage.semantic", input)?;
    let snapshot: CompleteProjectSnapshot = serde_json::from_value(output.clone()).map_err(|error| {
        PluginError::new(
            "invalid_plugin_invoke_response",
            format!(
                "invalid AppCore project snapshot `{output}`; expected one complete graph snapshot: {error}"
            ),
            false,
        )
    })?;
    if snapshot.project_root == project_root {
        return Ok(snapshot);
    }
    Err(PluginError::new(
        "invalid_plugin_invoke_response",
        format!(
            "AppCore project snapshot root `{}` differs from requested `{project_root}`; expected one project",
            snapshot.project_root
        ),
        false,
    ))
}

pub(crate) fn load(
    context: &mut PluginContext<'_>,
    project_root: &str,
    root_element_id: Option<&str>,
) -> Result<SemanticContext, PluginError> {
    let snapshot = complete_project_snapshot(context, project_root, None)?;
    let mut semantic = SemanticContext {
        elements: snapshot.elements,
        relationships: snapshot.relationships,
        artifacts: decode_semantic_artifacts(snapshot.artifacts)?,
    };
    if let Some(root) = root_element_id {
        retain_subtree(&mut semantic, root, project_root)?;
    }
    Ok(semantic)
}

fn decode_semantic_artifacts(records: Vec<Value>) -> Result<Vec<SemanticArtifact>, PluginError> {
    records
        .into_iter()
        .map(|record| {
            serde_json::from_value(record.clone()).map_err(|error| {
                PluginError::new(
                    "invalid_plugin_invoke_response",
                    format!("invalid AppCore semantic artifact `{record}`: {error}"),
                    false,
                )
            })
        })
        .collect()
}

pub(crate) fn retain_subtree(
    semantic: &mut SemanticContext,
    root_element_id: &str,
    project_root: &str,
) -> Result<(), PluginError> {
    if !semantic
        .elements
        .iter()
        .any(|element| element.semantic_element_id == root_element_id)
    {
        return Err(PluginError::new(
            "invalid_plugin_invoke_response",
            format!(
                "semantic element `{root_element_id}` is absent from project `{project_root}`; expected subtree root"
            ),
            false,
        ));
    }
    let roots = BTreeSet::from([root_element_id.to_owned()]);
    let ids = descendant_ids(&semantic.elements, &roots);
    semantic
        .elements
        .retain(|element| ids.contains(&element.semantic_element_id));
    semantic.relationships.retain(|relationship| {
        ids.contains(&relationship.source_element_id)
            || ids.contains(&relationship.target_element_id)
    });
    semantic
        .artifacts
        .retain(|artifact| ids.contains(&artifact.semantic_element_id));
    Ok(())
}

pub(crate) fn descendant_ids(
    elements: &[SemanticElement],
    root_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut ids = root_ids.clone();
    loop {
        let before = ids.len();
        for element in elements {
            if element
                .parent_element_id
                .as_ref()
                .is_some_and(|parent| ids.contains(parent))
            {
                ids.insert(element.semantic_element_id.clone());
            }
        }
        if ids.len() == before {
            return ids;
        }
    }
}
