use crate::local::sql::connections::SqlConnections;
use crate::local::sql::validation::require_non_empty;
use crate::{
    DbError, PluginDataMutation, PluginDataMutationResult, PluginDataRowRecord, PluginDataRowsPage,
    PluginDataTableRecord, Result,
};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// Maximum row operations accepted by one atomic plugin-storage call.
pub const MAX_PLUGIN_DATA_MUTATIONS: usize = 250_000;
/// Maximum serialized mutation payload accepted by one atomic call.
pub const MAX_PLUGIN_DATA_MUTATION_BYTES: usize = 128 * 1024 * 1024;

pub struct PluginDataRepository<'db> {
    conn: &'db SqlConnections,
}

impl<'db> PluginDataRepository<'db> {
    pub(crate) fn new(conn: &'db SqlConnections) -> Self {
        Self { conn }
    }

    /// Creates or updates one plugin-owned logical data table.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// db.plugin_data().ensure_table("builtin.assistant", "cache", &serde_json::json!({})).unwrap();
    /// ```
    pub fn ensure_table(
        &self,
        plugin_id: &str,
        table_name: &str,
        schema: &Value,
    ) -> Result<PluginDataTableRecord> {
        validate_table_ref(plugin_id, table_name)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.write_conn();
        conn.execute(
            "INSERT INTO plugin_data_tables(plugin_id, table_name, schema_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(plugin_id, table_name) DO UPDATE SET
             schema_json = excluded.schema_json, updated_at = excluded.updated_at",
            params![plugin_id, table_name, serde_json::to_string(schema)?, now],
        )?;
        table_by_name(&conn, plugin_id, table_name)?
            .ok_or_else(|| DbError::invalid_value(table_name, "plugin data table row"))
    }

    /// Lists the invoking plugin's logical tables in deterministic name order.
    pub fn tables(&self, plugin_id: &str) -> Result<Vec<PluginDataTableRecord>> {
        require_non_empty(plugin_id, "non-empty plugin id")?;
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare_cached(
            "SELECT plugin_id, table_name, schema_json, created_at, updated_at
             FROM plugin_data_tables WHERE plugin_id = ?1 ORDER BY table_name",
        )?;
        statement
            .query_map(params![plugin_id], table_from_row)?
            .map(|row| table_record_from_parts(row?))
            .collect()
    }

    /// Stores one row in a plugin-owned logical table.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// db.plugin_data().ensure_table("builtin.assistant", "cache", &serde_json::json!({})).unwrap();
    /// db.plugin_data().put_row("builtin.assistant", "cache", "latest", &serde_json::json!({"ok": true})).unwrap();
    /// ```
    pub fn put_row(
        &self,
        plugin_id: &str,
        table_name: &str,
        row_key: &str,
        value: &Value,
    ) -> Result<PluginDataRowRecord> {
        validate_row_ref(plugin_id, table_name, row_key)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.write_conn();
        conn.execute(
            "INSERT INTO plugin_data_rows(plugin_id, table_name, row_key, value_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(plugin_id, table_name, row_key) DO UPDATE SET
             value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![plugin_id, table_name, row_key, serde_json::to_string(value)?, now],
        )?;
        row_by_key(&conn, plugin_id, table_name, row_key)?
            .ok_or_else(|| DbError::invalid_value(row_key, "plugin data row"))
    }

    /// Reads one row from a plugin-owned logical table.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// assert!(db.plugin_data().row("builtin.assistant", "cache", "missing").unwrap().is_none());
    /// ```
    pub fn row(
        &self,
        plugin_id: &str,
        table_name: &str,
        row_key: &str,
    ) -> Result<Option<PluginDataRowRecord>> {
        validate_row_ref(plugin_id, table_name, row_key)?;
        let conn = self.conn.read_conn();
        row_by_key(&conn, plugin_id, table_name, row_key)
    }

    /// Lists rows for one plugin-owned logical table.
    ///
    /// # Example
    ///
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// assert!(db.plugin_data().rows("builtin.assistant", "cache").unwrap().is_empty());
    /// ```
    pub fn rows(&self, plugin_id: &str, table_name: &str) -> Result<Vec<PluginDataRowRecord>> {
        validate_table_ref(plugin_id, table_name)?;
        let conn = self.conn.read_conn();
        let mut stmt = conn.prepare_cached(
            "SELECT plugin_id, table_name, row_key, value_json, created_at, updated_at
             FROM plugin_data_rows WHERE plugin_id = ?1 AND table_name = ?2 ORDER BY row_key",
        )?;
        let rows = stmt.query_map(params![plugin_id, table_name], row_from_row)?;
        rows.map(|row| row_record_from_parts(row?)).collect()
    }

    /// Lists a bounded row-key page with optional literal prefix filtering.
    ///
    /// `after_key` is exclusive. `next_after_key` is the cursor for the next
    /// request and is absent when the page exhausts the matching rows.
    ///
    /// # Example
    /// ```
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// let page = db.plugin_data()
    ///     .rows_page("plugin.example", "records", Some("element:"), None, 100)
    ///     .unwrap();
    /// assert!(page.rows.is_empty());
    /// ```
    pub fn rows_page(
        &self,
        plugin_id: &str,
        table_name: &str,
        key_prefix: Option<&str>,
        after_key: Option<&str>,
        limit: usize,
    ) -> Result<PluginDataRowsPage> {
        validate_table_ref(plugin_id, table_name)?;
        if !(1..=1_000).contains(&limit) {
            return Err(DbError::invalid_value(
                limit.to_string(),
                "plugin data page limit from 1 through 1000",
            ));
        }
        let prefix = key_prefix.unwrap_or_default();
        let after = after_key.unwrap_or_default();
        let fetch_limit = i64::try_from(limit + 1)
            .map_err(|_| DbError::invalid_value(limit.to_string(), "SQLite page limit"))?;
        let conn = self.conn.read_conn();
        let mut stmt = conn.prepare_cached(
            "SELECT plugin_id, table_name, row_key, value_json, created_at, updated_at
             FROM plugin_data_rows
             WHERE plugin_id = ?1 AND table_name = ?2
               AND row_key > ?3
               AND substr(row_key, 1, length(?4)) = ?4
             ORDER BY row_key LIMIT ?5",
        )?;
        let mapped = stmt.query_map(
            params![plugin_id, table_name, after, prefix, fetch_limit],
            row_from_row,
        )?;
        let mut rows = mapped
            .map(|row| row_record_from_parts(row?))
            .collect::<Result<Vec<_>>>()?;
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_after_key = has_more.then(|| rows[rows.len() - 1].row_key.clone());
        Ok(PluginDataRowsPage {
            rows,
            next_after_key,
        })
    }

    /// Retains only the newest lexicographic row keys in one logical table.
    pub fn trim_rows_by_key(
        &self,
        plugin_id: &str,
        table_name: &str,
        retained_rows: usize,
    ) -> Result<usize> {
        validate_table_ref(plugin_id, table_name)?;
        if !(1..=1_000_000).contains(&retained_rows) {
            return Err(DbError::invalid_value(
                retained_rows.to_string(),
                "retained plugin row count from 1 through 1000000",
            ));
        }
        let retained_rows = i64::try_from(retained_rows)
            .map_err(|_| DbError::invalid_value(retained_rows.to_string(), "SQLite row limit"))?;
        let conn = self.conn.write_conn();
        Ok(conn.execute(
            "DELETE FROM plugin_data_rows
             WHERE plugin_id = ?1 AND table_name = ?2 AND row_key NOT IN (
                 SELECT row_key FROM plugin_data_rows
                 WHERE plugin_id = ?1 AND table_name = ?2
                 ORDER BY row_key DESC LIMIT ?3
             )",
            params![plugin_id, table_name, retained_rows],
        )?)
    }

    /// Applies a bounded set of plugin-owned row changes in one transaction.
    ///
    /// Every referenced table must already exist under `plugin_id`. Validation
    /// or persistence failure rolls back the complete batch across all tables.
    ///
    /// # Example
    /// ```
    /// use lumvise_db_core::PluginDataMutation;
    /// let db = lumvise_db_core::DbCore::in_memory().unwrap();
    /// db.plugin_data().ensure_table("plugin.example", "records", &serde_json::json!({})).unwrap();
    /// let result = db.plugin_data().apply_mutations("plugin.example", &[
    ///     PluginDataMutation::Put { table_name: "records".into(), row_key: "a".into(), value: serde_json::json!({"ok": true}) }
    /// ]).unwrap();
    /// assert_eq!(result.rows_put, 1);
    /// ```
    pub fn apply_mutations(
        &self,
        plugin_id: &str,
        mutations: &[PluginDataMutation],
    ) -> Result<PluginDataMutationResult> {
        validate_mutation_batch(plugin_id, mutations)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.write_conn();
        let transaction = conn.transaction()?;
        require_mutation_tables(&transaction, plugin_id, mutations)?;
        let result = execute_mutations(&transaction, plugin_id, mutations, &now)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn validate_mutation_batch(plugin_id: &str, mutations: &[PluginDataMutation]) -> Result<()> {
    require_non_empty(plugin_id, "non-empty plugin id")?;
    if !(1..=MAX_PLUGIN_DATA_MUTATIONS).contains(&mutations.len()) {
        return Err(DbError::invalid_value(
            mutations.len().to_string(),
            format!("plugin data mutation count from 1 through {MAX_PLUGIN_DATA_MUTATIONS}"),
        ));
    }
    let bytes = serde_json::to_vec(mutations)?.len();
    if bytes > MAX_PLUGIN_DATA_MUTATION_BYTES {
        return Err(DbError::invalid_value(
            bytes.to_string(),
            format!(
                "plugin data mutation payload no larger than {MAX_PLUGIN_DATA_MUTATION_BYTES} bytes"
            ),
        ));
    }
    mutations.iter().try_for_each(validate_mutation)
}

fn validate_mutation(mutation: &PluginDataMutation) -> Result<()> {
    let (table_name, row_key) = match mutation {
        PluginDataMutation::Put {
            table_name,
            row_key,
            ..
        }
        | PluginDataMutation::Delete {
            table_name,
            row_key,
        } => (table_name, row_key),
    };
    validate_row_ref("batch-owner", table_name, row_key)
}

fn require_mutation_tables(
    conn: &Connection,
    plugin_id: &str,
    mutations: &[PluginDataMutation],
) -> Result<()> {
    for mutation in mutations {
        let table_name = mutation_table(mutation);
        if table_by_name(conn, plugin_id, table_name)?.is_none() {
            return Err(DbError::invalid_value(
                table_name,
                format!("existing plugin data table owned by `{plugin_id}`"),
            ));
        }
    }
    Ok(())
}

fn mutation_table(mutation: &PluginDataMutation) -> &str {
    match mutation {
        PluginDataMutation::Put { table_name, .. }
        | PluginDataMutation::Delete { table_name, .. } => table_name,
    }
}

fn execute_mutations(
    conn: &Connection,
    plugin_id: &str,
    mutations: &[PluginDataMutation],
    now: &str,
) -> Result<PluginDataMutationResult> {
    let mut result = PluginDataMutationResult {
        rows_put: 0,
        rows_deleted: 0,
    };
    for mutation in mutations {
        execute_mutation(conn, plugin_id, mutation, now, &mut result)?;
    }
    Ok(result)
}

fn execute_mutation(
    conn: &Connection,
    plugin_id: &str,
    mutation: &PluginDataMutation,
    now: &str,
    result: &mut PluginDataMutationResult,
) -> Result<()> {
    match mutation {
        PluginDataMutation::Put {
            table_name,
            row_key,
            value,
        } => {
            conn.execute(
                "INSERT INTO plugin_data_rows(plugin_id, table_name, row_key, value_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(plugin_id, table_name, row_key) DO UPDATE SET
                 value_json = excluded.value_json, updated_at = excluded.updated_at",
                params![plugin_id, table_name, row_key, serde_json::to_string(value)?, now],
            )?;
            result.rows_put += 1;
        }
        PluginDataMutation::Delete {
            table_name,
            row_key,
        } => {
            result.rows_deleted += conn.execute(
                "DELETE FROM plugin_data_rows WHERE plugin_id = ?1 AND table_name = ?2 AND row_key = ?3",
                params![plugin_id, table_name, row_key],
            )?;
        }
    }
    Ok(())
}

fn table_by_name(
    conn: &Connection,
    plugin_id: &str,
    table_name: &str,
) -> Result<Option<PluginDataTableRecord>> {
    let mut statement = conn.prepare_cached(
        "SELECT plugin_id, table_name, schema_json, created_at, updated_at
         FROM plugin_data_tables WHERE plugin_id = ?1 AND table_name = ?2",
    )?;
    statement
        .query_row(params![plugin_id, table_name], table_from_row)
        .optional()?
        .map(table_record_from_parts)
        .transpose()
}

fn row_by_key(
    conn: &Connection,
    plugin_id: &str,
    table_name: &str,
    row_key: &str,
) -> Result<Option<PluginDataRowRecord>> {
    let mut statement = conn.prepare_cached(
        "SELECT plugin_id, table_name, row_key, value_json, created_at, updated_at
         FROM plugin_data_rows WHERE plugin_id = ?1 AND table_name = ?2 AND row_key = ?3",
    )?;
    statement
        .query_row(params![plugin_id, table_name, row_key], row_from_row)
        .optional()?
        .map(row_record_from_parts)
        .transpose()
}

fn validate_table_ref(plugin_id: &str, table_name: &str) -> Result<()> {
    require_non_empty(plugin_id, "non-empty plugin id")?;
    require_non_empty(table_name, "non-empty plugin data table name")
}

fn validate_row_ref(plugin_id: &str, table_name: &str, row_key: &str) -> Result<()> {
    validate_table_ref(plugin_id, table_name)?;
    require_non_empty(row_key, "non-empty plugin data row key")
}

fn table_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String, String)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn table_record_from_parts(
    parts: (String, String, String, String, String),
) -> Result<PluginDataTableRecord> {
    Ok(PluginDataTableRecord {
        plugin_id: parts.0,
        table_name: parts.1,
        schema: serde_json::from_str(&parts.2)?,
        created_at: parts.3,
        updated_at: parts.4,
    })
}

type PluginDataRowParts = (String, String, String, String, String, String);

fn row_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginDataRowParts> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn row_record_from_parts(parts: PluginDataRowParts) -> Result<PluginDataRowRecord> {
    Ok(PluginDataRowRecord {
        plugin_id: parts.0,
        table_name: parts.1,
        row_key: parts.2,
        value: serde_json::from_str(&parts.3)?,
        created_at: parts.4,
        updated_at: parts.5,
    })
}

#[cfg(test)]
mod tests {
    use super::MAX_PLUGIN_DATA_MUTATIONS;
    use crate::PluginDataMutation;
    use crate::local::runtime::DbCore;
    use serde_json::json;

    #[test]
    fn rows_page_filters_literal_prefix_and_advances_exclusive_cursor() {
        let db = DbCore::in_memory().expect("database");
        let repository = db.plugin_data();
        repository
            .ensure_table("plugin.alpha", "records", &json!({}))
            .expect("table");
        for key in ["element:a", "element:b", "element:c", "relation:a"] {
            repository
                .put_row("plugin.alpha", "records", key, &json!({"key": key}))
                .expect("row");
        }

        let first = repository
            .rows_page("plugin.alpha", "records", Some("element:"), None, 2)
            .expect("first page");
        let second = repository
            .rows_page(
                "plugin.alpha",
                "records",
                Some("element:"),
                first.next_after_key.as_deref(),
                2,
            )
            .expect("second page");

        assert_eq!(
            first
                .rows
                .iter()
                .map(|row| row.row_key.as_str())
                .collect::<Vec<_>>(),
            vec!["element:a", "element:b"]
        );
        assert_eq!(first.next_after_key.as_deref(), Some("element:b"));
        assert_eq!(second.rows[0].row_key, "element:c");
        assert!(second.next_after_key.is_none());
    }

    #[test]
    fn rows_page_rejects_unbounded_limit() {
        let db = DbCore::in_memory().expect("database");
        let error = db
            .plugin_data()
            .rows_page("plugin.alpha", "records", None, None, 0)
            .expect_err("zero limit rejected");

        assert!(error.to_string().contains("1 through 1000"));
    }

    #[test]
    fn apply_mutations_rolls_back_all_tables_when_one_table_is_missing() {
        let db = DbCore::in_memory().expect("database");
        let repository = db.plugin_data();
        repository
            .ensure_table("plugin.alpha", "elements", &json!({}))
            .expect("elements table");
        repository
            .put_row("plugin.alpha", "elements", "old", &json!({"version": 1}))
            .expect("old row");
        let mutations = vec![
            PluginDataMutation::Put {
                table_name: "elements".into(),
                row_key: "new".into(),
                value: json!({"version": 2}),
            },
            PluginDataMutation::Put {
                table_name: "missing".into(),
                row_key: "invalid".into(),
                value: json!({}),
            },
        ];

        repository
            .apply_mutations("plugin.alpha", &mutations)
            .expect_err("missing table must reject batch");

        assert!(
            repository
                .row("plugin.alpha", "elements", "new")
                .expect("read new")
                .is_none()
        );
    }

    #[test]
    fn apply_mutations_deletes_only_invoking_plugin_rows() {
        let db = DbCore::in_memory().expect("database");
        let repository = db.plugin_data();
        for plugin_id in ["plugin.alpha", "plugin.beta"] {
            repository
                .ensure_table(plugin_id, "records", &json!({}))
                .expect("table");
            repository
                .put_row(plugin_id, "records", "same", &json!({"owner": plugin_id}))
                .expect("row");
        }
        let result = repository
            .apply_mutations(
                "plugin.alpha",
                &[PluginDataMutation::Delete {
                    table_name: "records".into(),
                    row_key: "same".into(),
                }],
            )
            .expect("delete alpha");

        assert_eq!(result.rows_deleted, 1);
        assert!(
            repository
                .row("plugin.beta", "records", "same")
                .expect("beta row")
                .is_some()
        );
    }

    #[test]
    fn apply_mutations_rejects_total_count_over_quota_without_writes() {
        let db = DbCore::in_memory().expect("database");
        let repository = db.plugin_data();
        repository
            .ensure_table("plugin.alpha", "records", &json!({}))
            .expect("table");
        let mutations = (0..=MAX_PLUGIN_DATA_MUTATIONS)
            .map(|index| PluginDataMutation::Delete {
                table_name: "records".into(),
                row_key: format!("row-{index}"),
            })
            .collect::<Vec<_>>();

        let error = repository
            .apply_mutations("plugin.alpha", &mutations)
            .expect_err("over-quota batch must fail");

        assert!(
            error
                .to_string()
                .contains(&MAX_PLUGIN_DATA_MUTATIONS.to_string())
        );
    }
}
