use lumvise_contracts::{
    CreateKnowledgeArtifactRequestV2 as CreateRequest,
    UpdateKnowledgeArtifactRequestV2 as UpdateRequest,
};
use lumvise_plugin_sdk::PluginError;
use serde::Deserialize;

pub use lumvise_contracts::{
    KnowledgeArtifactKindV2 as KnowledgeKind, KnowledgeArtifactV2 as KnowledgeArtifact,
};

pub(crate) fn knowledge_type_label(kind: &KnowledgeKind) -> &'static str {
    match kind {
        KnowledgeKind::Specification => "specification",
        KnowledgeKind::Issue => "issue",
        KnowledgeKind::TaskAssignment => "task-assignment",
        KnowledgeKind::Definition => "definition",
        KnowledgeKind::Annotation => "annotation",
        KnowledgeKind::Report => "report",
        KnowledgeKind::Decision => "decision",
        KnowledgeKind::ManualNote => "manual-note",
        KnowledgeKind::DerivedSummary => "derived-summary",
    }
}

#[derive(Deserialize)]
pub(crate) struct SearchRequest {
    pub(crate) query: String,
    pub(crate) project_root: Option<String>,
    pub(crate) semantic_element_id: Option<String>,
    pub(crate) knowledge_type: Option<KnowledgeKind>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

pub(crate) fn create(request: CreateRequest) -> Result<KnowledgeArtifact, PluginError> {
    require(&request.artifact_id, "artifact_id")?;
    require(&request.semantic_element_id, "semantic_element_id")?;
    require(&request.title, "title")?;
    require(&request.content, "content")?;
    lumvise_contracts::validate_artifact_dependencies(&request.artifact_id, &request.dependencies)
        .map_err(|error| PluginError::new("invalid_knowledge_input", error, false))?;
    Ok(KnowledgeArtifact {
        artifact_id: request.artifact_id,
        semantic_element_id: request.semantic_element_id,
        knowledge_type: request.knowledge_type,
        title: request.title,
        content: request.content,
        tags: request.tags,
        dependencies: request.dependencies,
        metadata: request.metadata,
        path: request.path,
        project_root: request.project_root,
    })
}

pub(crate) fn update(
    mut artifact: KnowledgeArtifact,
    request: UpdateRequest,
) -> Result<KnowledgeArtifact, PluginError> {
    replace_nonempty(
        &mut artifact.semantic_element_id,
        request.semantic_element_id,
        "semantic_element_id",
    )?;
    if let Some(value) = request.knowledge_type {
        artifact.knowledge_type = value;
    }
    replace_nonempty(&mut artifact.title, request.title, "title")?;
    replace_nonempty(&mut artifact.content, request.content, "content")?;
    if let Some(dependencies) = request.dependencies {
        artifact.dependencies = dependencies;
    }
    lumvise_contracts::validate_artifact_dependencies(
        &artifact.artifact_id,
        &artifact.dependencies,
    )
    .map_err(|error| PluginError::new("invalid_knowledge_input", error, false))?;
    artifact.tags = request.tags.unwrap_or(artifact.tags);
    artifact.metadata = request.metadata.unwrap_or(artifact.metadata);
    artifact.path = request.path.or(artifact.path);
    artifact.project_root = request.project_root.or(artifact.project_root);
    Ok(artifact)
}

pub(crate) fn filter_matches(artifact: &KnowledgeArtifact, request: &SearchRequest) -> bool {
    matches_filter(
        artifact,
        request.project_root.as_ref(),
        request.semantic_element_id.as_ref(),
        request.knowledge_type.as_ref(),
    )
}

/// Shared search/list filter over the fields both listings carry; `None`
/// filters match everything. Project scope is exact equality on the record's
/// `project_root` — the semantics `search_knowledge` has always applied.
pub(crate) fn matches_filter(
    artifact: &KnowledgeArtifact,
    project_root: Option<&String>,
    semantic_element_id: Option<&String>,
    knowledge_type: Option<&KnowledgeKind>,
) -> bool {
    project_root.is_none_or(|value| artifact.project_root.as_ref() == Some(value))
        && semantic_element_id.is_none_or(|value| &artifact.semantic_element_id == value)
        && knowledge_type.is_none_or(|value| &artifact.knowledge_type == value)
}

/// Input of the `list_knowledge` export: every field optional.
#[derive(Deserialize)]
pub(crate) struct ListAllRequest {
    pub(crate) semantic_element_ids: Option<std::collections::HashSet<String>>,
    pub(crate) project_root: Option<String>,
    pub(crate) knowledge_type: Option<KnowledgeKind>,
    pub(crate) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

/// Applies the `list_knowledge` filters in memory: project scope, knowledge
/// type, tags (any of the requested tags), then the result limit.
pub(crate) fn filter_artifacts(
    artifacts: Vec<KnowledgeArtifact>,
    request: &ListAllRequest,
) -> Vec<KnowledgeArtifact> {
    let mut filtered: Vec<KnowledgeArtifact> = artifacts
        .into_iter()
        .filter(|artifact| {
            matches_filter(
                artifact,
                request.project_root.as_ref(),
                None,
                request.knowledge_type.as_ref(),
            ) && request
                .semantic_element_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&artifact.semantic_element_id))
                && request.tags.as_ref().is_none_or(|tags| {
                    tags.iter()
                        .any(|tag| artifact.tags.iter().any(|own| own == tag))
                })
        })
        .collect();
    if let Some(limit) = request.limit {
        filtered.truncate(limit);
    }
    filtered
}

pub(crate) fn require(value: &str, field: &str) -> Result<(), PluginError> {
    if !value.trim().is_empty() {
        return Ok(());
    }
    Err(PluginError::new(
        "invalid_knowledge_input",
        format!("invalid `{field}` value `{value}`; expected non-empty string"),
        false,
    ))
}

fn replace_nonempty(
    current: &mut String,
    replacement: Option<String>,
    field: &str,
) -> Result<(), PluginError> {
    if let Some(value) = replacement {
        require(&value, field)?;
        *current = value;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn artifact(value: Value) -> KnowledgeArtifact {
        serde_json::from_value(value).expect("decode KnowledgeArtifactV2")
    }

    #[test]
    fn list_all_filter_applies_project_scope_type_tag_and_limit() {
        let artifacts = vec![
            artifact(json!({
                "artifact_id": "c4", "semantic_element_id": "file:src/a.rs",
                "knowledge_type": "report", "title": "T", "content": "",
                "tags": ["scoped-c4", "nucleus"], "dependencies": [],
                "metadata": {}, "project_root": "/p1"
            })),
            artifact(json!({
                "artifact_id": "canvas", "semantic_element_id": "folder:src",
                "knowledge_type": "definition", "title": "T", "content": "",
                "tags": ["canvas"], "dependencies": [],
                "metadata": {}, "project_root": "/p1"
            })),
            artifact(json!({
                "artifact_id": "other", "semantic_element_id": "file:lib.rs",
                "knowledge_type": "report", "title": "T", "content": "",
                "tags": ["scoped-c4"], "dependencies": [],
                "metadata": {}, "project_root": "/p2"
            })),
            artifact(json!({
                "artifact_id": "unscoped", "semantic_element_id": "file:lib.rs",
                "knowledge_type": "report", "title": "T", "content": "",
                "tags": ["scoped-c4"], "dependencies": [],
                "metadata": {}, "project_root": null
            })),
        ];
        let no_filter = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: None,
            limit: None,
        };
        assert_eq!(filter_artifacts(artifacts.clone(), &no_filter).len(), 4);

        // Project scope: records whose `project_root` equals the argument.
        // Unscoped records carry no project scope and do not match a root
        // filter — the same semantics `search_knowledge` applies.
        let scoped = ListAllRequest {
            semantic_element_ids: None,
            project_root: Some("/p1".into()),
            knowledge_type: None,
            tags: None,
            limit: None,
        };
        assert_eq!(
            filter_artifacts(artifacts.clone(), &scoped)
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["c4", "canvas"]
        );

        let typed = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: Some(serde_json::from_value(json!("definition")).expect("valid kind")),
            tags: None,
            limit: None,
        };
        assert_eq!(
            filter_artifacts(artifacts.clone(), &typed)
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["canvas"]
        );

        // Tags match when the record carries any requested tag.
        let tagged = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: Some(vec!["canvas".into(), "absent".into()]),
            limit: None,
        };
        assert_eq!(
            filter_artifacts(artifacts.clone(), &tagged)
                .iter()
                .map(|artifact| artifact.artifact_id.as_str())
                .collect::<Vec<_>>(),
            vec!["canvas"]
        );

        let limited = ListAllRequest {
            semantic_element_ids: None,
            project_root: None,
            knowledge_type: None,
            tags: None,
            limit: Some(2),
        };
        assert_eq!(filter_artifacts(artifacts, &limited).len(), 2);
    }
}
