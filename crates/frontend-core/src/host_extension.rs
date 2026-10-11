//! The seam through which an embedding product offers its own services to compiled plugins.
//! It is always compiled: the app core routes these calls with or without a desktop shell.

/// A service the embedding product offers to compiled plugins under a name it
/// chooses. The app only routes the call through the `host.extension`
/// capability; a product that installs none leaves every extension unavailable.
///
/// # Example
/// ```
/// struct Echo;
/// impl lumvise_frontend_core::HostExtension for Echo {
///     fn invoke(
///         &self,
///         _plugin_id: &str,
///         input: serde_json::Value,
///     ) -> Result<serde_json::Value, String> {
///         Ok(input)
///     }
/// }
/// ```
pub trait HostExtension: Send + Sync {
    /// Handles one plugin request. An `Err` text reaches the plugin as the failure message.
    fn invoke(
        &self,
        plugin_id: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}
