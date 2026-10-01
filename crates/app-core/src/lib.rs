//! App Core coordinates Lumvise capability and Plugin interfaces.

mod app;
mod async_runtime;
mod endpoints;
mod error;
pub mod observability;
mod plugin;
mod project_execution;
mod runtime;
mod semantic_snapshot;
mod workspace_activity;

#[cfg(feature = "desktop-app")]
pub use app::AppCoreDesktopBridge;
pub use app::ScopedMcpHttpServer;
pub use app::run_headless_app;
#[cfg(feature = "desktop-app")]
pub use app::run_lumvise_app;
pub use app::runtime_coordinator::{
    AcquireResult, ActivationRequest, AppRuntimeCoordinator, OwnerLease, QuitRequest,
    RuntimeConnection, RuntimeControlPort, RuntimeCoordinatorError, RuntimeLauncher,
};
pub use async_runtime::{
    AppCoreRuntime, AppCoreRuntimeHealth, RuntimeFrontendHandle, RuntimeLlmHandle,
    RuntimeModalityHandle, RuntimeSpawnMode, RuntimeWorkerState,
};
pub use endpoints::{
    DatabaseEndpoints, DesktopBroadcastRecord, FrontendInteractionEndpoints, LlmEndpoints,
    ModalityEndpoints, ScreenFrameBroadcastRecord, ScreenshotRecord, VoiceTranscriptionRecord,
};
pub use error::{AppCoreError, Result};
pub use lumvise_frontend_core::{CanvasElement, CanvasPatch, CanvasRevisionRecord, CanvasSnapshot};
pub use lumvise_plugin_runtime::UninstallPolicy as CompiledPluginUninstallPolicy;
pub use plugin::{
    AppCoreHostCapabilityBroker, ChangeHookCycleReport, ChangeHookRun, LUMVISE_PLUGIN_ROOT_ENV,
    PLUGIN_MCP_MESSAGE_ENDPOINT, PLUGIN_MCP_SSE_ENDPOINT, PluginEndpoints, PluginInvocationRequest,
    PluginInvocationResponse, PluginInvocationStatus, PluginMcpTool, PluginProductionConfig,
    PluginRecurringTask, PluginRecurringTaskRun, SCOPED_MCP_MESSAGE_ENDPOINT,
    SCOPED_MCP_SSE_ENDPOINT, ScopedMcpChannel, ScopedMcpMessageRequest, ScopedMcpToolRoute,
    compiled_host_capability_versions,
};
pub use project_execution::{
    PROJECT_EXECUTION_HEARTBEAT_ENDPOINT, PROJECT_EXECUTION_NEXT_ENDPOINT,
    PROJECT_EXECUTION_PROGRESS_ENDPOINT, PROJECT_EXECUTION_REGISTER_ENDPOINT,
    PROJECT_EXECUTION_RESULT_ENDPOINT, PROJECT_EXECUTION_UNREGISTER_ENDPOINT,
    ProjectExecutionCommand, ProjectExecutionError, ProjectExecutionJob, ProjectExecutionPriority,
    ProjectExecutionRequest, ProjectExecutionService, ProjectExecutionStatus, SemanticArtifactTask,
};
pub use runtime::AppCore;
pub use semantic_snapshot::{
    SemanticSnapshotFailure, SemanticSnapshotFailurePhase, SemanticSnapshotOperation,
    SemanticSnapshotService, SemanticSnapshotStatus,
};
