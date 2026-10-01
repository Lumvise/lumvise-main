//! Settings adapter for the app-owned plugin lifecycle. The HTTP entrypoint is
//! `plugin_settings_response`; activation and persistence remain owned by AppCore.

use lumvise_plugin_runtime::{PluginRegistry, PluginRegistryRecord};
use semver::Version;
use serde::{Deserialize, Serialize};

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::AppCore;

const PLUGIN_SETTINGS_ENDPOINT: &str = "/api/settings/plugins";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledPluginStatus {
    plugin_id: String,
    version: String,
    publisher_id: String,
    enabled: bool,
    active: bool,
    required: bool,
    update_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginActivationRequest {
    plugin_id: String,
    version: String,
    enabled: bool,
}

/// Routes authenticated Settings requests, e.g. `GET /api/settings/plugins`.
pub(crate) fn plugin_settings_response(
    app: &AppCore,
    request: &HttpRequest,
) -> Option<HttpResponse> {
    if request.path != PLUGIN_SETTINGS_ENDPOINT {
        return None;
    }
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return Some(error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        ));
    }
    Some(match request.method.as_str() {
        "GET" => plugin_snapshot_response(app),
        "POST" => change_plugin_activation(app, &request.body),
        _ => error_response(
            "405 Method Not Allowed",
            format!("method {:?}; expected GET or POST", request.method),
        ),
    })
}

fn installed_plugin_statuses(app: &AppCore) -> crate::Result<Vec<InstalledPluginStatus>> {
    let registry = app.compiled_plugin_registry()?;
    newest_plugin_releases(&registry)?
        .into_iter()
        .map(|newest| installed_plugin_status(app, &registry, newest))
        .collect()
}

fn newest_plugin_releases(registry: &PluginRegistry) -> crate::Result<Vec<&PluginRegistryRecord>> {
    let mut releases = registry
        .records
        .iter()
        .map(|record| {
            Version::parse(&record.version)
                .map(|version| (record, version))
                .map_err(|_| {
                    crate::AppCoreError::invalid_value(&record.version, "a semantic plugin version")
                })
        })
        .collect::<crate::Result<Vec<_>>>()?;
    releases.sort_by(|(left, left_version), (right, right_version)| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| right_version.cmp(left_version))
    });
    releases.dedup_by(|(left, _), (right, _)| left.plugin_id == right.plugin_id);
    Ok(releases.into_iter().map(|(record, _)| record).collect())
}

fn installed_plugin_status(
    app: &AppCore,
    registry: &PluginRegistry,
    newest: &PluginRegistryRecord,
) -> crate::Result<InstalledPluginStatus> {
    // Historical packages are rollback candidates, not separate plugins.
    let selected = registry
        .records
        .iter()
        .find(|record| record.plugin_id == newest.plugin_id && record.enabled)
        .unwrap_or(newest);
    Ok(InstalledPluginStatus {
        plugin_id: selected.plugin_id.clone(),
        version: selected.version.clone(),
        publisher_id: selected.publisher_id.clone(),
        enabled: selected.enabled,
        active: selected.enabled && app.plugin_system().is_active(&selected.plugin_id)?,
        required: AppCore::is_required_compiled_plugin(&selected.plugin_id),
        update_version: (selected.version != newest.version).then(|| newest.version.clone()),
    })
}

fn plugin_snapshot_response(app: &AppCore) -> HttpResponse {
    match installed_plugin_statuses(app) {
        Ok(plugins) => json_response("200 OK", serde_json::json!({"plugins": plugins})),
        Err(error) => error_response("503 Service Unavailable", error.to_string()),
    }
}

fn change_plugin_activation(app: &AppCore, body: &[u8]) -> HttpResponse {
    let input = match serde_json::from_slice::<PluginActivationRequest>(body) {
        Ok(input) => input,
        Err(error) => {
            return error_response(
                "400 Bad Request",
                format!(
                    "invalid plugin activation request; expected {{pluginId: string, version: string, enabled: boolean}}: {error}"
                ),
            );
        }
    };
    if let Err(error) = apply_plugin_activation(app, &input) {
        return error_response("409 Conflict", error.to_string());
    }
    plugin_snapshot_response(app)
}

fn apply_plugin_activation(app: &AppCore, input: &PluginActivationRequest) -> crate::Result<()> {
    let registry = app.compiled_plugin_registry()?;
    if !registry
        .records
        .iter()
        .any(|record| record.plugin_id == input.plugin_id && record.version == input.version)
    {
        return Err(crate::AppCoreError::invalid_value(
            format!("{}@{}", input.plugin_id, input.version),
            "an installed plugin version",
        ));
    }
    if input.enabled {
        let newest = newest_plugin_releases(&registry)?
            .into_iter()
            .find(|record| record.plugin_id == input.plugin_id);
        if let Some(newest) = newest.filter(|record| record.version != input.version) {
            return Err(crate::AppCoreError::invalid_value(
                format!("{}@{}", input.plugin_id, input.version),
                format!(
                    "the current installed release {}@{}",
                    newest.plugin_id, newest.version
                ),
            ));
        }
        app.enable_compiled_plugin(&input.plugin_id, &input.version)
    } else {
        app.disable_compiled_plugin(&input.plugin_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn plugin_settings_requires_credentials_and_validates_the_request() {
        let app = AppCore::in_memory().unwrap();
        let mut request = HttpRequest {
            method: "POST".into(),
            path: PLUGIN_SETTINGS_ENDPOINT.into(),
            query: BTreeMap::new(),
            authorization: None,
            body: b"{}".to_vec(),
        };
        assert_eq!(
            plugin_settings_response(&app, &request).unwrap().status,
            "401 Unauthorized"
        );
        app.install_bridge_credential_store(std::sync::Arc::default())
            .unwrap();
        app.set_bridge_credential(Some("plugin-settings-test".into()), u64::MAX)
            .unwrap();
        request.authorization = Some("Bearer plugin-settings-test".into());
        assert_eq!(
            plugin_settings_response(&app, &request).unwrap().status,
            "400 Bad Request"
        );
        request.method = "DELETE".into();
        assert_eq!(
            plugin_settings_response(&app, &request).unwrap().status,
            "405 Method Not Allowed"
        );
        request.method = "GET".into();
        assert_eq!(
            plugin_settings_response(&app, &request).unwrap().status,
            "503 Service Unavailable"
        );
        request.path = "/api/unrelated".into();
        assert!(plugin_settings_response(&app, &request).is_none());
    }
}
