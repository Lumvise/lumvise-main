use serde_json::Value;

use crate::{PluginContext, PluginError};

/// Business interface implemented by one compiled plugin executable.
///
/// Implementations are shared by concurrent invocation workers and therefore
/// must be safe to access from multiple threads.
pub trait PluginApplication: Sync {
    /// Returns the stable plugin identifier declared by the package manifest.
    fn plugin_id(&self) -> &str;

    /// Dispatches one exported capability without exposing transport concerns.
    ///
    /// Use [`PluginContext::host_call`] for permission-gated host capabilities.
    /// Return [`PluginError::unknown_capability`] when `capability_id` is not exported.
    fn dispatch(
        &self,
        capability_id: &str,
        input: Value,
        context: &mut PluginContext<'_>,
    ) -> Result<Value, PluginError>;
}
