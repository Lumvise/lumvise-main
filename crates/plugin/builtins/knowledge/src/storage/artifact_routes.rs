//! Projects current routing facts onto read results without changing canonical artifact bodies.
//! Storage calls these batch decoders; project ownership is verified through existing indexed reads.
use super::*;

pub(super) fn project_rows(
    context: &mut PluginContext<'_>,
    root: &str,
    mut rows: Vec<Value>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    let allowed = verified_owner_ids(context, root, &rows)?;
    rows.retain(|row| {
        row["semantic_element_id"]
            .as_str()
            .is_some_and(|id| allowed.contains(id))
    });
    decode_graph_knowledge_artifacts(&rows, root)
}

fn verified_owner_ids(
    context: &mut PluginContext<'_>,
    root: &str,
    rows: &[Value],
) -> Result<HashSet<String>, PluginError> {
    let ids = owner_ids(rows)?;
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(elements_by_ids(context, root, &ids)?
        .into_iter()
        .filter(|owner| owner.project_root == root)
        .map(|owner| owner.semantic_element_id)
        .collect())
}

fn owner_ids(rows: &[Value]) -> Result<HashSet<String>, PluginError> {
    rows.iter()
        .filter(|row| is_knowledge_artifact(row))
        .map(|row| {
            row["semantic_element_id"]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| invalid_response("a non-empty artifact owner ID", row))
        })
        .collect()
}

pub(super) fn routed_rows(
    context: &mut PluginContext<'_>,
    rows: &[Value],
    mut routes: BTreeMap<String, String>,
) -> Result<Vec<KnowledgeArtifact>, PluginError> {
    rows.iter()
        .filter(|row| is_knowledge_artifact(row))
        .map(|row| {
            let mut artifact: KnowledgeArtifact = decode_graph_artifact(row)?;
            artifact.project_root = Some(cached_route(
                context,
                &mut routes,
                &artifact.semantic_element_id,
            )?);
            Ok(artifact)
        })
        .collect()
}

fn cached_route(
    context: &mut PluginContext<'_>,
    routes: &mut BTreeMap<String, String>,
    owner: &str,
) -> Result<String, PluginError> {
    if let Some(root) = routes.get(owner) {
        return Ok(root.clone());
    }
    let root = revision_reads::artifact_project_root(context, owner)?;
    routes.insert(owner.into(), root.clone());
    Ok(root)
}
