use chrono::Utc;
use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

use crate::{models::GraphRequest, parse, storage};

pub(crate) fn providers(
    _input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let roots = storage::graph_project_roots(context)?;
    let now = Utc::now().to_rfc3339();
    let providers = roots
        .into_iter()
        .map(|root| {
            let name = root
                .rsplit('/')
                .find(|part| !part.is_empty())
                .unwrap_or(&root);
            json!({
                "id": root, "name": name, "displayName": name, "projectRoot": root,
                "dashboardUrl": null, "semanticGraphUrl": null,
                "registeredAt": now, "lastSeenAt": now, "status": "indexed",
                "providerInstanceIds": []
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"providers": providers}))
}

pub(crate) fn semantic_graph(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: GraphRequest = parse(input, "semantic graph request")?;
    let project_root = graph_root(&request)?;
    let projection = storage::project_renderer_graph(
        context,
        storage::RendererGraphRequest {
            project_root: project_root.clone(),
            target_path: request.target_path.clone(),
            granularity: request.granularity,
            recursive: request.recursive.unwrap_or(true),
            include_external: request.include_external.unwrap_or(false),
            include_first_neighbors: request.include_first_neighbors.unwrap_or(false),
        },
    )?;
    let name = project_root
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(&project_root);
    let node_count = projection.nodes.len();
    let edge_count = projection.edges.len();
    Ok(json!({
        "providerId": request.provider_id.or_else(|| Some(project_root.clone())),
        "providerName": name,
        "dashboardUrl": null,
        "projectRoot": project_root,
        "targetPath": request.target_path,
        "granularity": request.granularity,
        "source": "app-owned-index",
        "generatedAt": Utc::now().to_rfc3339(),
        "commitVersion": projection.commit_version,
        "publishedAt": projection.published_at,
        "summary": format!("Semantic graph for {project_root}: {node_count} nodes, {edge_count} edges"),
        "nodes": projection.nodes,
        "edges": projection.edges
    }))
}

fn graph_root(request: &GraphRequest) -> Result<String, PluginError> {
    request
        .project_root
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            request
                .provider_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            PluginError::new(
                "semantic_project_not_found",
                "projectRoot or providerId is required; expected indexed semantic project scope"
                    .to_owned(),
                false,
            )
        })
}
