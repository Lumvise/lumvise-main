//! Durable MCP server instance registry persisted in SQLite.
//!
//! Wire shapes live in `lumvise-contracts::mcp`; this module stores their
//! fields verbatim (capabilities and control channel as JSON strings) so the
//! HTTP layer can round-trip contract types without translation loss.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::interface::McpInstance;
use crate::{DbError, Result};

const ACTIVE_WINDOW_SECONDS: i64 = 60;

/// Registration input carrying contract fields.
#[derive(Debug, Clone, Copy)]
pub struct NewMcpInstance<'a> {
    pub instance_id: &'a str,
    pub project_root: &'a str,
    pub display_name: &'a str,
    pub capabilities_json: &'a str,
    pub control_channel_json: Option<&'a str>,
}

/// Upserts one instance, preserving `created_at` on re-registration.
pub(crate) fn register_mcp_instance(
    conn: &Connection,
    instance: &NewMcpInstance<'_>,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.prepare_cached(
        "INSERT INTO mcp_instances(
            instance_id, project_root, display_name, status, capabilities_json,
            control_channel_json, last_heartbeat_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, 'starting', ?4, ?5, ?6, ?6, ?6)
         ON CONFLICT(instance_id) DO UPDATE SET
            project_root = excluded.project_root,
            display_name = excluded.display_name,
            status = 'starting',
            capabilities_json = excluded.capabilities_json,
            control_channel_json = excluded.control_channel_json,
            last_heartbeat_at = excluded.last_heartbeat_at,
            updated_at = excluded.updated_at",
    )?
    .execute(params![
        instance.instance_id,
        instance.project_root,
        instance.display_name,
        instance.capabilities_json,
        instance.control_channel_json,
        now,
    ])?;
    Ok(())
}

/// Updates heartbeat + status; the instance must belong to the given project.
pub(crate) fn heartbeat_mcp_instance(
    conn: &Connection,
    instance_id: &str,
    project_root: &str,
    status: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let updated = conn
        .prepare_cached(
            "UPDATE mcp_instances
             SET status = ?3, last_heartbeat_at = ?4, updated_at = ?4
             WHERE instance_id = ?1 AND project_root = ?2",
        )?
        .execute(params![instance_id, project_root, status, now])?;
    if updated != 1 {
        return Err(DbError::invalid_value(
            instance_id,
            "registered MCP instance for project",
        ));
    }
    Ok(())
}

/// Reads one instance row or fails with a typed error.
pub(crate) fn required_mcp_instance(conn: &Connection, instance_id: &str) -> Result<McpInstance> {
    conn.prepare_cached(&select_sql("WHERE instance_id = ?1"))?
        .query_row(params![instance_id], instance_row)
        .optional()?
        .ok_or_else(|| DbError::invalid_value(instance_id, "registered MCP instance"))
}

/// Returns instances with a heartbeat inside the active window, newest first.
pub(crate) fn active_mcp_instances(conn: &Connection) -> Result<Vec<McpInstance>> {
    let cutoff = (Utc::now() - chrono::Duration::seconds(ACTIVE_WINDOW_SECONDS)).to_rfc3339();
    let mut statement = conn.prepare_cached(&select_sql(
        "WHERE last_heartbeat_at > ?1 ORDER BY updated_at DESC",
    ))?;
    let rows = statement.query_map(params![cutoff], instance_row)?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

/// Returns every registered instance, newest first.
pub(crate) fn all_mcp_instances(conn: &Connection) -> Result<Vec<McpInstance>> {
    let mut statement = conn.prepare_cached(&select_sql("ORDER BY updated_at DESC"))?;
    let rows = statement.query_map([], instance_row)?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

/// Removes one instance registration.
pub(crate) fn deregister_mcp_instance(conn: &Connection, instance_id: &str) -> Result<()> {
    conn.prepare_cached("DELETE FROM mcp_instances WHERE instance_id = ?1")?
        .execute(params![instance_id])?;
    Ok(())
}

fn select_sql(suffix: &str) -> String {
    format!(
        "SELECT instance_id, project_root, display_name, status, capabilities_json,
                control_channel_json, last_heartbeat_at, created_at, updated_at
         FROM mcp_instances {suffix}"
    )
}

fn instance_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpInstance> {
    Ok(McpInstance {
        instance_id: row.get(0)?,
        project_root: row.get(1)?,
        display_name: row.get(2)?,
        status: row.get(3)?,
        capabilities_json: row.get(4)?,
        control_channel_json: row.get(5)?,
        last_heartbeat_at: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local::sql::schema::initialize_sql_schema(&conn).unwrap();
        conn
    }

    fn new_instance<'a>(instance_id: &'a str, project_root: &'a str) -> NewMcpInstance<'a> {
        NewMcpInstance {
            instance_id,
            project_root,
            display_name: "worker",
            capabilities_json: r#"["semantic_indexing"]"#,
            control_channel_json: None,
        }
    }

    #[test]
    fn register_heartbeat_and_list_roundtrip() {
        let conn = memory_conn();
        register_mcp_instance(&conn, &new_instance("mcp-1", "/repo")).unwrap();
        let registered = required_mcp_instance(&conn, "mcp-1").unwrap();
        assert_eq!(registered.status, "starting");
        heartbeat_mcp_instance(&conn, "mcp-1", "/repo", "ready").unwrap();
        let active = active_mcp_instances(&conn).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].status, "ready");
    }

    #[test]
    fn reregistration_preserves_created_at() {
        let conn = memory_conn();
        register_mcp_instance(&conn, &new_instance("mcp-1", "/repo")).unwrap();
        let first = required_mcp_instance(&conn, "mcp-1").unwrap();
        register_mcp_instance(&conn, &new_instance("mcp-1", "/other")).unwrap();
        let second = required_mcp_instance(&conn, "mcp-1").unwrap();
        assert_eq!(first.created_at, second.created_at);
        assert_eq!(second.project_root, "/other");
    }

    #[test]
    fn heartbeat_rejects_wrong_project() {
        let conn = memory_conn();
        register_mcp_instance(&conn, &new_instance("mcp-1", "/repo")).unwrap();
        assert!(heartbeat_mcp_instance(&conn, "mcp-1", "/wrong", "ready").is_err());
    }

    #[test]
    fn deregister_removes_instance() {
        let conn = memory_conn();
        register_mcp_instance(&conn, &new_instance("mcp-1", "/repo")).unwrap();
        deregister_mcp_instance(&conn, "mcp-1").unwrap();
        assert!(all_mcp_instances(&conn).unwrap().is_empty());
    }
}
