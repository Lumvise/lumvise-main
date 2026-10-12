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

/// Product-owned account for hosted AI services: where they live and how to authenticate.
///
/// # Example
/// ```
/// #[derive(Debug)]
/// struct Account;
/// impl lumvise_frontend_core::HostedAiAccount for Account {
///     fn endpoint(&self) -> String {
///         "https://example.com/api/public/v1/ai".to_string()
///     }
///     fn bearer_token(&self, _force_refresh: bool) -> Result<String, String> {
///         Ok("token".to_string())
///     }
/// }
/// ```
pub trait HostedAiAccount: Send + Sync + std::fmt::Debug {
    /// Base URL of the hosted services, e.g. "https://example.com/api/public/v1/ai".
    fn endpoint(&self) -> String;
    /// A currently valid bearer token; `force_refresh` after an authentication failure.
    fn bearer_token(&self, force_refresh: bool) -> Result<String, String>;
}
