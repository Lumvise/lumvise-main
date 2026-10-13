//! This computer's optional source-folder bindings; logical graph roots never become disk paths.
use super::*;
use lumvise_db_core::{RelationalOperation, RelationalPersistence, RelationalResult};
use serde::Deserialize;
const SCOPE: &str = "project-source-folders";
pub(crate) const ENDPOINT: &str = "/api/project-source-folder";

pub(super) fn bound_folder(
    relational: &dyn RelationalPersistence,
    root: &str,
    control: &InvocationControl,
) -> Result<Option<String>, String> {
    match relational
        .execute(
            RelationalOperation::GetPersistentSetting {
                scope: SCOPE.into(),
                key: root.into(),
            },
            control,
        )
        .map_err(|error| error.to_string())?
    {
        RelationalResult::PersistentSetting(record) => {
            Ok(record.and_then(|record| record.value.as_str().map(str::to_owned)))
        }
        _ => Err("Source folder: expected a persistent setting".into()),
    }
}
pub(crate) fn response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        );
    }
    match folder_request(app, request) {
        Ok(folder) => json_response("200 OK", json!({"folder":folder})),
        Err(message) => error_response("400 Bad Request", message),
    }
}
fn folder_request(app: &AppCore, request: &HttpRequest) -> Result<Option<String>, String> {
    let root = non_empty(request, "project_root").ok_or("project_root is required")?;
    let id = root
        .strip_prefix("lumvise://projects/")
        .ok_or("Source folder binding requires an online project root")?;
    if !uuid::Uuid::parse_str(id).is_ok_and(|id| format!("lumvise://projects/{id}") == root) {
        return Err(format!(
            "project_root {root}: expected lumvise://projects/<UUID>"
        ));
    }
    let control = InvocationControl::sixty_seconds();
    if request.method == "GET" {
        return bound_folder(app.relational.as_ref(), &root, &control);
    }
    app.semantic
        .execute(
            SemanticOperation::ProjectIdentity {
                project_root: root.clone(),
            },
            &control,
        )
        .map_err(|error| error.to_string())?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Binding {
        folder: Option<String>,
    }
    let binding: Binding = serde_json::from_slice(&request.body)
        .map_err(|error| format!("Source folder: expected {{folder: path or null}}: {error}"))?;
    let folder = binding
        .folder
        .as_deref()
        .map(canonical_directory)
        .transpose()?;
    app.relational
        .execute(
            RelationalOperation::SetPersistentSetting {
                scope: SCOPE.into(),
                key: root,
                value: json!(folder),
            },
            &control,
        )
        .map_err(|error| error.to_string())?;
    Ok(folder)
}
fn canonical_directory(folder: &str) -> Result<String, String> {
    let path = std::fs::canonicalize(folder).map_err(|error| {
        format!("Folder {folder:?}: expected an accessible local directory: {error}")
    })?;
    if !path.is_dir() {
        return Err(format!("Folder {folder:?}: expected a directory"));
    }
    Ok(path.to_string_lossy().into_owned())
}
