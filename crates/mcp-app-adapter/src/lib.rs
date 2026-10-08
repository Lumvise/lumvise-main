//! Thin MCP transport adapter for one running Lumvise app runtime.
//!
//! App Core owns application state and plugin execution. This crate only
//! discovers its HTTP bridge and translates generic MCP requests to that seam.

mod app_bridge;
mod application;
mod product;
mod project_execution;
mod session_project_binding;
mod tool_catalog;

pub use app_bridge::AppBridgeConfig;
pub use application::{McpAppConfig, open_server};
pub use product::{run_lumvise, run_lumvise_with_runtime};
pub use project_execution::{ProjectExecutionConfig, ProjectExecutionProvider};

/// Ensures a ready app generation and returns its credential-bound bridge.
pub fn ensure_runtime(
    config: &AppBridgeConfig,
    deadline: std::time::Instant,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<lumvise_app_core::RuntimeConnection, String> {
    match config.coordinator().ensure_ready(
        lumvise_app_core::ActivationRequest {
            background: true,
            ..Default::default()
        },
        deadline,
        cancelled,
    ) {
        Ok(connection) => Ok(connection),
        Err(lumvise_app_core::RuntimeCoordinatorError::Quitting) => {
            config.mark_terminal();
            Err(lumvise_app_core::RuntimeCoordinatorError::Quitting.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}
