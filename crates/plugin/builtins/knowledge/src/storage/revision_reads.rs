//! Exact project-scoped reads preserve the distinction between a working edit and retained history.
use super::*;
use lumvise_semantic_revisions::{
    SemanticRecordRef, SemanticRevisionBody, SemanticRevisionResolution,
};

/// Lists bodies and their working versions at one commit without a per-artifact read.
/// Example: observe_knowledge(context, |context| list_project_knowledge(context, root)).
pub(crate) fn observe_knowledge(
    context: &mut PluginContext<'_>,
    read: impl FnOnce(&mut PluginContext<'_>) -> Result<Vec<KnowledgeArtifact>, PluginError>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let before = crate::semantic_context::current_revision(context)?;
    let mut artifacts = read(context)?;
    attach_selected_versions(context, &mut artifacts)?;
    let after = crate::semantic_context::current_revision(context)?;
    if before != after {
        return Err(PluginError::new(
            "knowledge_read_conflict",
            format!(
                "artifact read ended at commit {after}; expected captured commit {before}. Retry the read without changing a write base"
            ),
            true,
        ));
    }
    Ok(artifacts)
}

fn attach_selected_versions(
    context: &mut PluginContext<'_>,
    artifacts: &mut [KnowledgeArtifact],
) -> Result<(), PluginError> {
    for (root, ids) in artifact_projects(artifacts)? {
        let edits = selected_versions(context, &root, ids)?;
        for artifact in artifacts
            .iter_mut()
            .filter(|artifact| artifact.project_root.as_deref() == Some(&root))
        {
            artifact.edit_version = edits.get(&artifact.artifact_id).copied();
        }
    }
    Ok(())
}

fn artifact_projects(
    artifacts: &[KnowledgeArtifact],
) -> Result<BTreeMap<String, Vec<String>>, PluginError> {
    let mut projects = BTreeMap::<String, Vec<String>>::new();
    for artifact in artifacts {
        let root = artifact
            .project_root
            .as_ref()
            .ok_or_else(|| invalid_response("a routed artifact project", &json!(artifact)))?;
        projects
            .entry(root.clone())
            .or_default()
            .push(artifact.artifact_id.clone());
    }
    Ok(projects)
}

fn selected_versions(
    context: &mut PluginContext<'_>,
    root: &str,
    ids: Vec<String>,
) -> Result<BTreeMap<String, u64>, PluginError> {
    let output = context.host_call(SEMANTIC_CAPABILITY, json!({"operation":"revision_selection_members","query":{
        "project_root":root,"selection_id":"working","structure_coverage":{"kind":"none"},"artifact_ids":ids
    }}))?;
    let membership: Option<lumvise_semantic_revisions::SemanticRevisionSelectionObservation> =
        crate::parse(
            output["membership"].clone(),
            "scoped artifact versions or null",
        )?;
    Ok(membership
        .map(|value| value.expected.artifact_edits)
        .unwrap_or_default())
}

pub(crate) fn read_revision(
    context: &mut PluginContext<'_>,
    root: &str,
    record: SemanticRecordRef,
) -> Result<Option<SemanticRevisionResolution>, PluginError> {
    let output = context.host_call(
        SEMANTIC_CAPABILITY,
        json!({"operation":"resolve_revision", "project_root":root, "record":record}),
    )?;
    crate::parse(
        output["revision"].clone(),
        "an exact project-scoped revision or null",
    )
}

pub(crate) fn artifact_at_edit(
    context: &mut PluginContext<'_>,
    root: &str,
    id: &str,
    version: u64,
) -> Result<KnowledgeArtifact, PluginError> {
    let revision = read_revision(context, root, SemanticRecordRef::artifact(id))?
        .ok_or_else(|| unavailable_edit(id, version))?;
    if revision
        .artifact_state
        .as_ref()
        .is_none_or(|state| state.edit_version != version)
    {
        return Err(unavailable_edit(id, version));
    }
    let SemanticRevisionBody::Artifact(artifact) = revision.body else {
        return Err(unavailable_edit(id, version));
    };
    if artifact.artifact_id != id {
        return Err(unavailable_edit(id, version));
    }
    let mut body = json!(artifact);
    body["project_root"] = json!(root);
    body["edit_version"] = json!(version);
    decode_graph_artifact(&body)
}

pub(super) fn current_knowledge_body(
    context: &mut PluginContext<'_>,
    original: Value,
) -> Result<Value, PluginError> {
    let owner_id = original["semantic_element_id"]
        .as_str()
        .ok_or_else(|| invalid_response("an artifact owner ID", &original))?;
    let root = artifact_project_root(context, owner_id)?;
    let id = original["artifact_id"]
        .as_str()
        .ok_or_else(|| unavailable_edit("", 0))?;
    let Some(revision) = read_revision(context, &root, SemanticRecordRef::artifact(id))? else {
        let mut body = original;
        body["project_root"] = json!(root);
        return Ok(body);
    };
    let SemanticRevisionBody::Artifact(artifact) = revision.body else {
        return Err(unavailable_edit(id, 0));
    };
    let mut body = json!(artifact);
    body["project_root"] = json!(root);
    body["edit_version"] = json!(revision.artifact_state.map(|state| state.edit_version));
    Ok(body)
}

pub(super) fn artifact_project_root(
    context: &mut PluginContext<'_>,
    owner_id: &str,
) -> Result<String, PluginError> {
    let owner = required_semantic_element(context, owner_id)?;
    owner["project_root"]
        .as_str()
        .filter(|root| !root.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_response("a non-empty routed artifact owner project", &owner))
}

fn unavailable_edit(id: &str, version: u64) -> PluginError {
    PluginError::new(
        "knowledge_artifact_edit_unavailable",
        format!(
            "artifact `{id}` edit {version} is no longer available; expected the original accepted body. Read get_artifact_write_receipt for its durable acknowledgement; re-read the current artifact before a new edit"
        ),
        false,
    )
}
