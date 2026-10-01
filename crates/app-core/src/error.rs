use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppCoreError>;

#[derive(Debug, Error)]
pub enum AppCoreError {
    #[error(transparent)]
    Db(#[from] lumvise_db_core::DbError),

    #[error(transparent)]
    Frontend(#[from] lumvise_frontend_core::FrontendError),

    #[error(transparent)]
    Neural(#[from] lumvise_neural_core::NeuralError),

    #[error(transparent)]
    PluginRuntime(#[from] lumvise_plugin_runtime::PluginRuntimeError),

    #[error(transparent)]
    ControlledPluginRuntime(#[from] lumvise_plugin_runtime::PluginInvocationError),

    #[error(transparent)]
    PluginSandbox(#[from] lumvise_plugin_runtime::PluginSandboxError),

    #[error(transparent)]
    PluginPackage(#[from] lumvise_plugin_package::PackageError),

    #[error(transparent)]
    PluginReleaseIndex(#[from] lumvise_plugin_package::PluginReleaseIndexError),

    #[error(transparent)]
    PluginRepository(#[from] lumvise_plugin_runtime::PluginRepositoryError),

    #[error("invalid plugin production policy `{path}`: {message}")]
    PluginProductionPolicy {
        path: std::path::PathBuf,
        message: String,
    },

    #[error(
        "plugin MCP tool `{tool_name}` has conflicting owners {plugin_ids:?}; expected one owner"
    )]
    PluginMcpCollision {
        tool_name: String,
        plugin_ids: Vec<String>,
    },

    #[error(
        "compiled plugin HTTP method `{method}` has conflicting routes {route_owners:?}; expected one owner per request path"
    )]
    PluginHttpCollision {
        method: String,
        route_owners: Vec<String>,
    },

    #[error(
        "compiled plugin View `{view_id}` has conflicting owners {plugin_ids:?}; expected one owner"
    )]
    PluginViewCollision {
        view_id: String,
        plugin_ids: Vec<String>,
    },

    #[error("invalid value {value:?}; expected {expected}")]
    InvalidValue { value: String, expected: String },

    #[error("missing value {value:?}; expected {expected}")]
    MissingValue { value: String, expected: String },

    #[error("mutex {value:?} is poisoned; expected usable app-core state")]
    PoisonedMutex { value: String },

    #[error("unsupported capability {value:?}; expected {expected}")]
    UnsupportedCapability { value: String, expected: String },

    #[error("runtime worker {value:?} unavailable; expected {expected}")]
    RuntimeWorkerUnavailable { value: String, expected: String },
}

impl From<lumvise_neural_core::llm_providers::LlmFailure> for AppCoreError {
    fn from(error: lumvise_neural_core::llm_providers::LlmFailure) -> Self {
        use lumvise_neural_core::NeuralError;
        Self::Neural(NeuralError::ProviderFailed {
            provider_id: "llm-executor".to_string(),
            message: error.message,
        })
    }
}

impl AppCoreError {
    pub(crate) fn invalid_value(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::InvalidValue {
            value: value.into(),
            expected: expected.into(),
        }
    }

    pub(crate) fn missing_value(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::MissingValue {
            value: value.into(),
            expected: expected.into(),
        }
    }

    pub(crate) fn poisoned_mutex(value: impl Into<String>) -> Self {
        Self::PoisonedMutex {
            value: value.into(),
        }
    }

    pub(crate) fn unsupported(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::UnsupportedCapability {
            value: value.into(),
            expected: expected.into(),
        }
    }

    pub(crate) fn worker_unavailable(value: impl Into<String>) -> Self {
        Self::RuntimeWorkerUnavailable {
            value: value.into(),
            expected: "running worker".to_string(),
        }
    }
}
