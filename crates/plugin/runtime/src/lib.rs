//! Owns compiled plugin installation state and supervised subprocess lifecycle.
//!
//! App modules call [`PluginSystem`]. Process transport and catalog state remain
//! internal so no caller can bypass readiness, identity, or deadline checks.

#![deny(missing_docs)]

mod broker;
mod catalog;
mod error;
mod invocation;
mod process;
mod repository;
mod repository_config;
mod sandbox;
mod schema;
mod system;

pub use broker::{
    DenyAllHostCapabilityBroker, HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest,
};
pub use error::{PluginRuntimeError, SchemaDirection};
pub use invocation::{
    PluginInvocationCancellation, PluginInvocationCancellationRequest, PluginInvocationClass,
    PluginInvocationContext, PluginInvocationError, PluginInvocationFailureKind,
    PluginInvocationRequest,
};
pub use lumvise_plugin_package::ExportDescriptor;
pub use repository::{
    PluginRegistry, PluginRegistryRecord, PluginRepository, PluginRepositoryError,
    PluginRestoreFailure, PluginRestoreFailureKind, PluginRestoreReport, UninstallPolicy,
};
pub use repository_config::{LUMVISE_PLUGIN_ROOT_ENV, PluginRepositoryConfig};
pub use sandbox::{
    DenyExecutionSandbox, PluginSandbox, PluginSandboxError, PluginSandboxRequest,
    ProductionPluginSandbox,
};
pub use system::{
    BackgroundExportKind, ExclusiveInvocationLanes, ExclusiveLaneSnapshot, ExportConcurrencyPolicy,
    ExportConcurrencyRegistry, PLUGIN_INVOCATION_DEADLINE, PluginAdmissionSnapshot,
    PluginInvocationHandle, PluginRuntimeConfig, PluginSystem, PluginViewAsset,
    PublishedBackgroundExport, PublishedCommand, PublishedPlugin,
};
