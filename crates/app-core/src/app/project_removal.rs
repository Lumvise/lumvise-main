//! Removes a workspace project through the standing semantic lifecycle.
//!
//! The authenticated app route is the only entrypoint. Import coordination and
//! graph cleanup stay here; renderers never access persistence internals. Source
//! files and the database's change history are intentionally retained.

use lumvise_db_core::{ProjectSnapshotScope, SemanticOperation, SemanticResult};
use lumvise_resource_routing::InvocationControl;
use serde::{Deserialize, Serialize};

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::AppCore;

pub(crate) const PROJECT_REMOVAL_ENDPOINT: &str = "/api/project-removal";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectRemovalInput {
    project_root: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectRemovalResult {
    project_root: String,
    removed_elements: usize,
    removed_artifacts: usize,
}

/// Removes the named graph. Example body: `{"projectRoot":"/projects/demo"}`.
pub(crate) fn project_removal_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        );
    }
    let input = match removal_input(&request.body) {
        Ok(input) => input,
        Err(message) => return error_response("400 Bad Request", message),
    };
    match remove_project_graph(app, &input.project_root) {
        Ok(result) => json_response("200 OK", serde_json::json!(result)),
        Err(message) => error_response("500 Internal Server Error", message),
    }
}

fn removal_input(body: &[u8]) -> Result<ProjectRemovalInput, String> {
    let input: ProjectRemovalInput = serde_json::from_slice(body).map_err(|error| {
        format!("invalid project removal body; expected {{projectRoot: non-empty string}}: {error}")
    })?;
    if input.project_root.trim().is_empty() {
        return Err(format!(
            "invalid projectRoot `{}`; expected non-empty project root",
            input.project_root
        ));
    }
    Ok(input)
}

fn remove_project_graph(app: &AppCore, project_root: &str) -> Result<ProjectRemovalResult, String> {
    let mut sessions = app
        .project_import_sessions
        .lock()
        .map_err(|_| "project import sessions poisoned; expected available importer".to_owned())?;
    // Clear acknowledgements even after a partial failure, so explicit re-import
    // always publishes a fresh scan instead of accepting the removed snapshot.
    sessions.retain(|(_, imported_root), _| imported_root != project_root);
    let control = InvocationControl::sixty_seconds();
    let removed_elements = deactivate_project(app, project_root, &control)?;
    app.project_execution()
        .cancel_project(project_root)
        .map_err(|error| error.to_string())?;
    let removed_artifacts = remove_project_artifacts(app, project_root, &control)?;
    Ok(ProjectRemovalResult {
        project_root: project_root.to_owned(),
        removed_elements,
        removed_artifacts,
    })
}

fn deactivate_project(
    app: &AppCore,
    project_root: &str,
    control: &InvocationControl,
) -> Result<usize, String> {
    let removed_elements = active_project_elements(app, project_root, control)?;
    if removed_elements == 0 {
        return Ok(0);
    }
    // One authoritative empty snapshot removes the live graph in one publication.
    // Inactive owners also reject late artifact writes from background workers.
    let operation = SemanticOperation::SyncStructure {
        project_root: project_root.to_owned(),
        elements: Vec::new(),
        relationships: Vec::new(),
    };
    match app
        .semantic
        .execute(operation, control)
        .map_err(|error| error.to_string())?
    {
        SemanticResult::SyncStructure(_) => Ok(removed_elements),
        other => Err(format!(
            "invalid project deactivation result `{other:?}`; expected SyncStructure"
        )),
    }
}

fn active_project_elements(
    app: &AppCore,
    project_root: &str,
    control: &InvocationControl,
) -> Result<usize, String> {
    let operation = SemanticOperation::ProjectSnapshot {
        scope: ProjectSnapshotScope::ProjectRoot(project_root.to_owned()),
        artifact_namespace: None,
    };
    match app
        .semantic
        .execute(operation, control)
        .map_err(|error| error.to_string())?
    {
        SemanticResult::ProjectSnapshot(snapshot) => Ok(snapshot
            .elements
            .iter()
            .filter(|element| element.lifecycle != "inactive")
            .count()),
        other => Err(format!(
            "invalid project snapshot `{other:?}`; expected ProjectSnapshot"
        )),
    }
}

fn remove_project_artifacts(
    app: &AppCore,
    project_root: &str,
    control: &InvocationControl,
) -> Result<usize, String> {
    let operation = SemanticOperation::ProjectArtifacts {
        project_root: project_root.to_owned(),
        artifact_namespace: None,
    };
    let artifacts = match app
        .semantic
        .execute(operation, control)
        .map_err(|error| error.to_string())?
    {
        SemanticResult::Artifacts(artifacts) => artifacts,
        other => {
            return Err(format!(
                "invalid project artifacts result `{other:?}`; expected Artifacts"
            ));
        }
    };
    let mut removed = 0;
    for artifact in artifacts {
        let operation = SemanticOperation::RemoveArtifact {
            artifact_id: artifact.artifact_id,
        };
        match app
            .semantic
            .execute(operation, control)
            .map_err(|error| error.to_string())?
        {
            SemanticResult::Removed { removed: true } => removed += 1,
            SemanticResult::Removed { removed: false } => {}
            other => {
                return Err(format!(
                    "invalid artifact removal result `{other:?}`; expected Removed"
                ));
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
