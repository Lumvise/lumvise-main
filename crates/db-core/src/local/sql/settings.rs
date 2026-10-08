use crate::local::sql::connections::SqlConnections;
use crate::local::sql::validation::require_non_empty;
use crate::{Result, SettingRecord};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub struct PersistentSettingsRepository<'db> {
    conn: &'db SqlConnections,
}

impl<'db> PersistentSettingsRepository<'db> {
    pub(crate) fn new(conn: &'db SqlConnections) -> Self {
        Self { conn }
    }

    /// Stores JSON for a persistent setting.
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
    ///     RelationalOperation::SetPersistentSetting {
    ///         scope: "app".into(),
    ///         key: "theme".into(),
    ///         value: serde_json::json!("dark"),
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, RelationalResult::PersistentSettingUpdated(setting) if setting.scope == "app" && setting.key == "theme" && setting.value == serde_json::json!("dark")));
    /// ```
    pub fn set_json(&self, scope: &str, key: &str, value: &Value) -> Result<()> {
        require_non_empty(scope, "non-empty setting scope")?;
        require_non_empty(key, "non-empty setting key")?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.write_conn();
        conn.execute(
            "INSERT INTO persistent_settings(scope, key, value_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scope, key) DO UPDATE SET value_json = excluded.value_json,
             updated_at = excluded.updated_at",
            params![scope, key, serde_json::to_string(value)?, now],
        )?;
        Ok(())
    }

    /// Reads JSON for a persistent setting.
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
    ///     RelationalOperation::GetPersistentSetting { scope: "missing".into(), key: "key".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, RelationalResult::PersistentSetting(None)));
    /// ```
    pub fn get_json(&self, scope: &str, key: &str) -> Result<Option<SettingRecord>> {
        require_non_empty(scope, "non-empty setting scope")?;
        require_non_empty(key, "non-empty setting key")?;
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare_cached(
            "SELECT scope, key, value_json, updated_at FROM persistent_settings
             WHERE scope = ?1 AND key = ?2",
        )?;
        let row = statement
            .query_row(params![scope, key], setting_row_from_row)
            .optional()?;
        row.map(setting_record_from_parts).transpose()
    }
}

fn setting_row_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn setting_record_from_parts(parts: (String, String, String, String)) -> Result<SettingRecord> {
    Ok(SettingRecord {
        scope: parts.0,
        key: parts.1,
        value: serde_json::from_str(&parts.2)?,
        updated_at: parts.3,
    })
}
