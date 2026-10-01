//! Rust SDK for implementing compiled Lumvise plugin executables.
//!
//! Applications provide identity and dispatch only. The SDK owns the versioned process protocol,
//! package-bound handshake, session validation, host-call correlation, framing, and shutdown.
//!
//! # Example
//!
//! ```no_run
//! use lumvise_plugin_sdk::{PluginApplication, PluginContext, PluginError, run_stdio};
//! use serde_json::{Value, json};
//!
//! struct ExamplePlugin;
//!
//! impl PluginApplication for ExamplePlugin {
//!     fn plugin_id(&self) -> &str { "example.plugin" }
//!
//!     fn dispatch(
//!         &self,
//!         capability_id: &str,
//!         _input: Value,
//!         _context: &mut PluginContext<'_>,
//!     ) -> Result<Value, PluginError> {
//!         match capability_id {
//!             "example.manifest" => Ok(json!({"name": "Example"})),
//!             other => Err(PluginError::unknown_capability(other)),
//!         }
//!     }
//! }
//!
//! run_stdio(&ExamplePlugin)?;
//! # Ok::<(), lumvise_plugin_sdk::SdkError>(())
//! ```

#![deny(missing_docs)]

mod application;
mod context;
mod error;
mod session;

pub use application::PluginApplication;
#[cfg(feature = "test-support")]
pub use context::HostCallTransport;
pub use context::PluginContext;
pub use error::{PluginError, SdkError};
/// Typed current StorageTrigger request and response contracts.
pub use lumvise_plugin_protocol::{
    StorageTriggerChanged, StorageTriggerDisposition, StorageTriggerRequest, StorageTriggerResponse,
};
pub use session::{run, run_stdio};
