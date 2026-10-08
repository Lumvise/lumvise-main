use crate::local::sql::connections::SqlConnections;
use crate::local::sql::validation::require_non_empty;
use crate::{PluginSettingsRecord, Result};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub struct PluginSettingsRepository<'db> {
    conn: &'db SqlConnections,
}

impl<'db> PluginSettingsRepository<'db> {
    pub(crate) fn new(conn: &'db SqlConnections) -> Self {
        Self { conn }
    }

    /// Stores plugin configuration and enabled state.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, RelationalOperation, RelationalPersistence, RelationalResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = RelationalPersistence::execute(
    ///     &persistence,
    ///     RelationalOperation::SetPluginSetting {
    ///         plugin_id: "knowledge".into(),
    ///         enabled: true,
    ///         config: serde_json::json!({}),
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, RelationalResult::PluginSettingUpdated(setting) if setting.plugin_id == "knowledge" && setting.enabled && setting.config == serde_json::json!({})));
    /// ```
    pub fn set_config(&self, plugin_id: &str, enabled: bool, config: &Value) -> Result<()> {
        require_non_empty(plugin_id, "non-empty plugin id")?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.write_conn();
        conn.execute(
            "INSERT INTO plugin_settings(plugin_id, enabled, config_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(plugin_id) DO UPDATE SET enabled = excluded.enabled,
             config_json = excluded.config_json, updated_at = excluded.updated_at",
            params![plugin_id, enabled, serde_json::to_string(config)?, now],
        )?;
        Ok(())
    }

    /// Reads plugin configuration by id.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, RelationalOperation, RelationalPersistence, RelationalResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = RelationalPersistence::execute(
    ///     &persistence,
    ///     RelationalOperation::GetPluginSetting { plugin_id: "missing".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, RelationalResult::PluginSetting(None)));
    /// ```
    pub fn plugin_settings(&self, plugin_id: &str) -> Result<Option<PluginSettingsRecord>> {
        require_non_empty(plugin_id, "non-empty plugin id")?;
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare_cached(
            "SELECT plugin_id, enabled, config_json, updated_at FROM plugin_settings
             WHERE plugin_id = ?1",
        )?;
        let row = statement
            .query_row(params![plugin_id], plugin_settings_row_from_row)
            .optional()?;
        row.map(plugin_settings_from_parts).transpose()
    }
}

fn plugin_settings_row_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, i64, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn plugin_settings_from_parts(
    parts: (String, i64, String, String),
) -> Result<PluginSettingsRecord> {
    Ok(PluginSettingsRecord {
        plugin_id: parts.0,
        enabled: parts.1 != 0,
        config: serde_json::from_str(&parts.2)?,
        updated_at: parts.3,
    })
}
