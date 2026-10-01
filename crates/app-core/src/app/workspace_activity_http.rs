//! Credential-protected, in-memory activity reads for the workspace.
//! Reuses the existing buffered app bridge; one waiting request replaces idle polling.

use super::mcp_http::{HttpRequest, HttpResponse, error_response, json_response};
use crate::AppCore;
use std::time::Duration;

pub(crate) const ACTIVITY_ENDPOINT: &str = "/api/workspace/activity";

pub(crate) fn activity_response(app: &AppCore, request: &HttpRequest) -> HttpResponse {
    if crate::plugin::mcp_http_bridge::is_authenticated_bridge_request(app, request).is_err() {
        return error_response(
            "401 Unauthorized",
            "invalid or missing runtime bridge credential",
        );
    }
    let (root, revision) = match activity_query(request) {
        Ok(query) => query,
        Err(error) => return error_response("400 Bad Request", error),
    };
    let snapshot = app.plugin_host_services.activity().wait_for_changes(
        root,
        revision,
        Duration::from_secs(15),
    );
    json_response(
        "200 OK",
        serde_json::to_value(snapshot).expect("activity snapshot is serializable"),
    )
}

fn activity_query(request: &HttpRequest) -> Result<(&str, Option<u64>), String> {
    let root = request
        .query
        .get("project_root")
        .map(String::as_str)
        .unwrap_or_default();
    if root.trim().is_empty() {
        return Err(format!(
            "project_root `{root}`: expected a non-empty project root"
        ));
    }
    let revision = request
        .query
        .get("after_revision")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("after_revision `{value}`: expected a non-negative integer"))
        })
        .transpose()?;
    Ok((root, revision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn activity_route_checks_credentials_then_validates_cursor() {
        let app = AppCore::in_memory().unwrap();
        let request = HttpRequest {
            method: "GET".into(),
            path: ACTIVITY_ENDPOINT.into(),
            query: BTreeMap::new(),
            authorization: None,
            body: Vec::new(),
        };
        assert_eq!(activity_response(&app, &request).status, "401 Unauthorized");
        let mut request = request;
        request.query.insert("project_root".into(), "/repo".into());
        request.query.insert("after_revision".into(), "-1".into());
        assert!(activity_query(&request).unwrap_err().contains("-1"));
        request.query.remove("after_revision");
        assert_eq!(activity_query(&request).unwrap(), ("/repo", None));
    }

    #[test]
    fn native_app_bridge_reads_the_shared_activity_history() {
        let app = AppCore::in_memory().unwrap();
        app.install_bridge_credential_store(std::sync::Arc::default())
            .unwrap();
        let ticket = app
            .plugin_host_services
            .activity()
            .track_llm("builtin.assistant", "fake");
        ticket.started();
        let (status, body) = app.route_app_bridge_request(
            "GET",
            ACTIVITY_ENDPOINT,
            BTreeMap::from([("project_root".into(), "/repo".into())]),
            Vec::new(),
        );
        assert_eq!(status, 200, "{body}");
        let snapshot: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(snapshot["entries"][0]["kind"], "assistant");
        assert_eq!(snapshot["entries"][0]["status"], "running");
    }
}
