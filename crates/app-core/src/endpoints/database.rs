//! Plugin-neutral persistence endpoints.
//!
//! Semantic schemas and indexing rules belong to the Semantic plugin. Compiled
//! plugins access their private records through the namespaced storage host
//! capability; these endpoints expose only generic application persistence.

use crate::{AppCore, AppCoreError, Result};
use lumvise_db_core::{
    ArtifactBlob, ChangeHookScope, ChangesSinceRevisionPage, PluginSettingsRecord,
    RelationalOperation, RelationalResult, SemanticOperation, SemanticResult, SettingRecord,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;

pub struct DatabaseEndpoints<'app> {
    app: &'app AppCore,
}

impl<'app> DatabaseEndpoints<'app> {
    pub(crate) fn new(app: &'app AppCore) -> Self {
        Self { app }
    }

    /// Stores persistent JSON application settings.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.database().set_setting("app", "theme", &serde_json::json!("dark")).unwrap();
    /// ```
    pub fn set_setting(&self, scope: &str, key: &str, value: &Value) -> Result<SettingRecord> {
        match self.app.relational.execute(
            RelationalOperation::SetPersistentSetting {
                scope: scope.to_string(),
                key: key.to_string(),
                value: value.clone(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            RelationalResult::PersistentSettingUpdated(record) => Ok(record),
            other => Err(unexpected_result("set persistent setting", other)),
        }
    }

    /// Retrieves persistent JSON application settings.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.database().setting("app", "missing").unwrap().is_none());
    /// ```
    pub fn setting(&self, scope: &str, key: &str) -> Result<Option<SettingRecord>> {
        match self.app.relational.execute(
            RelationalOperation::GetPersistentSetting {
                scope: scope.to_string(),
                key: key.to_string(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            RelationalResult::PersistentSetting(record) => Ok(record),
            other => Err(unexpected_result("get persistent setting", other)),
        }
    }

    /// Stores generic plugin enablement and configuration.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.database().set_plugin_settings("example", true, &serde_json::json!({})).unwrap();
    /// ```
    pub fn set_plugin_settings(
        &self,
        plugin_id: &str,
        enabled: bool,
        config: &Value,
    ) -> Result<PluginSettingsRecord> {
        match self.app.relational.execute(
            RelationalOperation::SetPluginSetting {
                plugin_id: plugin_id.to_string(),
                enabled,
                config: config.clone(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            RelationalResult::PluginSettingUpdated(record) => Ok(record),
            other => Err(unexpected_result("set plugin setting", other)),
        }
    }
    /// Retrieves generic plugin configuration.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.database().plugin_settings("missing").unwrap().is_none());
    /// ```
    pub fn plugin_settings(&self, plugin_id: &str) -> Result<Option<PluginSettingsRecord>> {
        match self.app.relational.execute(
            RelationalOperation::GetPluginSetting {
                plugin_id: plugin_id.to_string(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            RelationalResult::PluginSetting(record) => Ok(record),
            other => Err(unexpected_result("get plugin setting", other)),
        }
    }

    /// Stores a binary blob without interpreting its domain.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// app.database().put_blob("blob://a", "a", "text/plain", b"x").unwrap();
    pub fn put_blob(
        &self,
        content_ref: &str,
        artifact_id: &str,
        media_type: &str,
        content: &[u8],
    ) -> Result<()> {
        match self.app.semantic.execute(
            SemanticOperation::ArtifactBlobPut {
                content_ref: content_ref.to_string(),
                artifact_id: artifact_id.to_string(),
                media_type: media_type.to_string(),
                content: content.to_vec(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ArtifactBlob(Some(_)) => Ok(()),
            SemanticResult::ArtifactBlob(None) => Err(AppCoreError::missing_value(
                content_ref,
                "blob persisted by semantic persistence",
            )),
            other => Err(unexpected_semantic_result("put artifact blob", other)),
        }
    }

    /// Retrieves a binary blob by content reference.
    ///
    /// # Example
    /// ```
    /// let app = lumvise_app_core::AppCore::in_memory().unwrap();
    /// assert!(app.database().blob("blob://missing").unwrap().is_none());
    pub fn blob(&self, content_ref: &str) -> Result<Option<ArtifactBlob>> {
        match self.app.semantic.execute(
            SemanticOperation::ArtifactBlobGet {
                content_ref: content_ref.to_string(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ArtifactBlob(blob) => Ok(blob),
            other => Err(unexpected_semantic_result("get artifact blob", other)),
        }
    }

    /// Reads one graph-derived, revision-cursor delta page for transient consumers.
    pub fn changes_since_revision(
        &self,
        scope: ChangeHookScope,
        after_revision: i64,
        limit: usize,
    ) -> Result<ChangesSinceRevisionPage> {
        match self.app.semantic.execute(
            SemanticOperation::ChangesSinceRevision {
                scope,
                after_revision,
                limit,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangesSinceRevision(page) => Ok(page),
            other => Err(unexpected_semantic_result("changes since revision", other)),
        }
    }
}

fn unexpected_result(operation: &str, result: RelationalResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("matching relational persistence result, got {result:?}"),
    )
}

fn unexpected_semantic_result(operation: &str, result: SemanticResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("matching semantic persistence result, got {result:?}"),
    )
}
