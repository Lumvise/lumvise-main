mod canvas_files;
#[cfg(feature = "desktop-app")]
mod desktop;
mod document_conversion;
#[cfg(feature = "assistant-e2e")]
pub(crate) mod e2e_control;
#[cfg(feature = "desktop-app")]
pub(crate) mod executor;
mod headless;
pub(crate) mod http_router;
pub(crate) mod managed_models;
pub(crate) mod mcp_http;
pub(crate) mod mcp_session_registry;
mod plugin_background_driver;
mod plugin_settings;
mod project_execution_http;
pub(crate) mod project_import;
mod project_removal;
mod provider_settings;
mod provider_startup;
pub(crate) mod resource_routing;
mod runtime_bridge;
pub(crate) mod runtime_coordinator;
mod source_file;
mod startup;
#[cfg(test)]
pub(crate) use canvas_files::read_canvas_file_with;
pub(crate) use canvas_files::{export_canvas_files, validate_media_type};
pub(crate) use source_file::invoke_project_source;
mod storage_change_stream;
mod workspace_activity_http;

#[cfg(feature = "desktop-app")]
pub use desktop::AppCoreDesktopBridge;
#[cfg(feature = "desktop-app")]
pub use executor::run_lumvise_app;
pub use headless::run_headless_app;
pub use mcp_http::ScopedMcpHttpServer;
#[cfg(all(feature = "assistant-e2e", feature = "desktop-app"))]
mod e2e_voice_adapters;

impl crate::AppCore {
    /// Routes one app-bridge HTTP request in-process, for callers that already
    /// are the app (the desktop renderer). The current runtime bridge
    /// credential is stamped on the request so credential-gated plugin routes
    /// behave exactly as they do over the socket; no credential ever leaves
    /// the process.
    pub fn route_app_bridge_request(
        &self,
        method: &str,
        path: &str,
        query: std::collections::BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> (u16, String) {
        let authorization = self
            .mint_bridge_credential()
            .ok()
            .map(|(credential, _)| format!("Bearer {credential}"));
        let response = http_router::route_http_request(
            self,
            mcp_http::HttpRequest {
                method: method.to_string(),
                path: path.to_string(),
                query,
                authorization,
                body,
            },
        );
        let status = response
            .status
            .split_whitespace()
            .next()
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(500);
        let body = response
            .buffered_bytes()
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
            .unwrap_or_default();
        (status, body)
    }
}
