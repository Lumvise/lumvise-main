use crate::{DbError, Result};
use chrono::Utc;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

// This release covers the SQLite/Grafeo pair, independently of app releases.
const DATABASE_FORMAT_RELEASE: i64 = 1;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS persistent_settings (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (scope, key)
);

CREATE TABLE IF NOT EXISTS plugin_settings (
    plugin_id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS semantic_commits (
    commit_version INTEGER PRIMARY KEY,
    state TEXT NOT NULL,
    failure_message TEXT,
    created_at TEXT NOT NULL,
    published_at TEXT
);

CREATE TABLE IF NOT EXISTS semantic_project_lineages (
    project_root TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS semantic_graph_projections (
    project_root TEXT PRIMARY KEY,
    commit_version INTEGER NOT NULL,
    projection_json BLOB NOT NULL,
    updated_at TEXT NOT NULL
);



CREATE TABLE IF NOT EXISTS semantic_project_lineages (
    project_root TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_blobs (
    content_ref TEXT PRIMARY KEY,
    artifact_id TEXT NOT NULL,
    media_type TEXT NOT NULL,
    content BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_artifact_blobs_artifact
    ON artifact_blobs(artifact_id);

CREATE TABLE IF NOT EXISTS vector_regeneration_jobs (
    project_root TEXT PRIMARY KEY,
    engine_id TEXT NOT NULL,
    model TEXT,
    status TEXT NOT NULL,
    processed INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_data_tables (
    plugin_id TEXT NOT NULL,
    table_name TEXT NOT NULL,
    schema_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(plugin_id, table_name)
);

CREATE TABLE IF NOT EXISTS plugin_data_rows (
    plugin_id TEXT NOT NULL,
    table_name TEXT NOT NULL,
    row_key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(plugin_id, table_name, row_key),
    FOREIGN KEY(plugin_id, table_name)
    REFERENCES plugin_data_tables(plugin_id, table_name)
);

CREATE INDEX IF NOT EXISTS idx_plugin_data_rows_table
ON plugin_data_rows(plugin_id, table_name, updated_at);

CREATE TABLE IF NOT EXISTS plugin_background_registrations (
    plugin_id TEXT NOT NULL,
    export_id TEXT NOT NULL,
    export_kind TEXT NOT NULL,
    contract_json TEXT NOT NULL,
    active INTEGER NOT NULL,
    next_due_at INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(plugin_id, export_id)
);

CREATE TABLE IF NOT EXISTS plugin_background_deliveries (
    delivery_id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    export_id TEXT NOT NULL,
    delivery_kind TEXT NOT NULL,
    source_key TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    lease_expires_at INTEGER,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(plugin_id, export_id, source_key),
    FOREIGN KEY(plugin_id, export_id)
    REFERENCES plugin_background_registrations(plugin_id, export_id)
);

CREATE INDEX IF NOT EXISTS idx_plugin_background_deliveries_due
ON plugin_background_deliveries(state, next_attempt_at, delivery_id);

CREATE INDEX IF NOT EXISTS idx_plugin_background_deliveries_export
ON plugin_background_deliveries(plugin_id, export_id, state, updated_at);

CREATE TABLE IF NOT EXISTS change_hook_registrations (
    hook_name TEXT PRIMARY KEY,
    project_root TEXT NOT NULL,
    entity_kinds_json TEXT NOT NULL,
    watermark INTEGER NOT NULL,
    registration_generation INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);


CREATE TABLE IF NOT EXISTS mcp_instances (
    instance_id TEXT PRIMARY KEY,
    project_root TEXT NOT NULL,
    display_name TEXT NOT NULL,
    status TEXT NOT NULL,
    capabilities_json TEXT NOT NULL,
    control_channel_json TEXT,
    last_heartbeat_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_mcp_instances_status
ON mcp_instances(status, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_mcp_instances_heartbeat
ON mcp_instances(last_heartbeat_at DESC);
"#;

pub(crate) fn initialize_sql_schema(conn: &Connection, graph_exists: bool) -> Result<()> {
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if !requires_initialization(&transaction, graph_exists)? {
        transaction.commit()?;
        return Ok(());
    }
    transaction.execute_batch(SCHEMA_SQL)?;
    transaction.execute(
        "INSERT OR IGNORE INTO semantic_commits(commit_version, state, created_at, published_at)
         VALUES (0, 'published', ?1, ?1)",
        params![Utc::now().to_rfc3339()],
    )?;
    transaction.pragma_update(None, "user_version", DATABASE_FORMAT_RELEASE)?;
    transaction.commit()?;
    Ok(())
}

fn requires_initialization(conn: &Connection, graph_exists: bool) -> Result<bool> {
    let stored: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if stored == DATABASE_FORMAT_RELEASE {
        return Ok(false);
    }
    if stored != 0 || graph_exists || has_user_schema(conn)? {
        return Err(DbError::UnsupportedDatabaseRelease {
            stored,
            required: DATABASE_FORMAT_RELEASE,
        });
    }
    Ok(true)
}

fn has_user_schema(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*')",
        [],
        |row| row.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_initialization_rolls_back_schema_and_release() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "max_page_count", 1).unwrap();
        assert!(initialize_sql_schema(&conn, false).is_err());
        let release: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(release, 0);
        assert!(!has_user_schema(&conn).unwrap());
        conn.pragma_update(None, "max_page_count", 1000).unwrap();
        initialize_sql_schema(&conn, false).unwrap();
        assert!(has_user_schema(&conn).unwrap());
    }

    #[test]
    fn concurrent_initializers_share_one_release() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lumvise.db");
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|threads| {
            let initialize = || {
                let conn = Connection::open(&path).unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                start.wait();
                initialize_sql_schema(&conn, false).unwrap();
            };
            threads.spawn(initialize);
            threads.spawn(initialize);
        });
        let conn = Connection::open(path).unwrap();
        let release: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(release, DATABASE_FORMAT_RELEASE);
    }
}
