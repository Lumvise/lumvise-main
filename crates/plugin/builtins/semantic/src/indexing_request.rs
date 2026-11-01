//! Validates the Indexing import request and delegates the import to the host.

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportInput {
    folder_path: String,
    #[serde(rename = "projectRoot")]
    _project_root: Option<String>,
    #[serde(rename = "replacePaths")]
    _replace_paths: Option<Vec<String>>,
    #[serde(rename = "importId")]
    _import_id: Option<String>,
    #[serde(rename = "cancel", default)]
    _cancel: bool,
    #[serde(rename = "statusOnly", default)]
    _status_only: bool,
}

/// Imports one folder through the host's authorized project importer.
pub(crate) fn index_project(
    input: Value,
    context: &mut PluginContext<'_>,
) -> Result<Value, PluginError> {
    let request: ImportInput = serde_json::from_value(input.clone()).map_err(|error| {
        PluginError::new(
            "invalid_indexing_input",
            format!(
                "import request `{input}`; expected object `{{folderPath, projectRoot?, replacePaths?}}`: {error}"
            ),
            false,
        )
    })?;
    if request.folder_path.trim().is_empty() {
        return Err(PluginError::new(
            "invalid_indexing_input",
            format!(
                "folderPath `{}`; expected a non-empty string",
                request.folder_path
            ),
            false,
        ));
    }
    context.host_call(
        "project.import",
        json!({"operation": "import_folder", "request": input}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SemanticPlugin;
    use lumvise_plugin_sdk::{HostCallTransport, PluginApplication};

    struct FakeImportHost {
        calls: Vec<(String, Value)>,
        result: Result<Value, PluginError>,
    }

    impl HostCallTransport for FakeImportHost {
        fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
            self.calls.push((capability.into(), input));
            self.result.clone()
        }
    }

    fn import(host: &mut FakeImportHost, request: Value) -> Result<Value, PluginError> {
        SemanticPlugin.dispatch("index_project", request, &mut PluginContext::for_test(host))
    }

    fn host(result: Result<Value, PluginError>) -> FakeImportHost {
        FakeImportHost {
            calls: Vec::new(),
            result,
        }
    }

    #[test]
    fn import_forwards_optional_fields_and_the_unchanged_host_result() {
        let result = json!({
            "projectRoot": "/accepted", "filesScanned": 2,
            "elementsUpserted": 3, "relationshipsUpserted": 4,
            "skipped": [{"path": "ignored", "reason": "excluded"}],
            "startedAt": "start", "finishedAt": "finish"
        });
        let requests = [
            json!({"folderPath": "/no-local-folder-needed"}),
            json!({"folderPath": "/no-local-folder-needed", "projectRoot": null, "replacePaths": null}),
            json!({"folderPath": "/no-local-folder-needed", "projectRoot": "/selected", "replacePaths": []}),
            json!({"folderPath": "/no-local-folder-needed", "replacePaths": ["src/main.rs"]}),
        ];
        for request in requests {
            let mut host = host(Ok(result.clone()));
            assert_eq!(import(&mut host, request.clone()).unwrap(), result);
            assert_eq!(
                host.calls,
                vec![(
                    "project.import".into(),
                    json!({"operation": "import_folder", "request": request})
                )]
            );
        }
    }

    #[test]
    fn import_rejects_invalid_requests_before_calling_the_host() {
        let inputs = [
            json!(null),
            json!({}),
            json!({"folderPath": null}),
            json!({"folderPath": 7}),
            json!({"folderPath": ""}),
            json!({"folderPath": " \t"}),
            json!({"folderPath": "/source", "projectRoot": 7}),
            json!({"folderPath": "/source", "replacePaths": "src"}),
            json!({"folderPath": "/source", "replacePaths": [1]}),
            json!({"folderPath": "/source", "plugin_id": "builtin.semantic"}),
        ];
        for input in inputs {
            let mut host = host(Ok(json!({})));
            let error = import(&mut host, input).unwrap_err();
            assert_eq!(error.code, "invalid_indexing_input");
            assert!(host.calls.is_empty());
        }
    }

    #[test]
    fn import_returns_the_host_error_unchanged() {
        let failure = PluginError::new("import_denied", "not authorized", true)
            .with_details(json!({"root": "/unavailable"}));
        let mut host = host(Err(failure.clone()));
        assert_eq!(
            import(&mut host, json!({"folderPath": "/unavailable"})).unwrap_err(),
            failure
        );
        assert_eq!(host.calls.len(), 1);
    }
}
