use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::{Uuid, Version};

use crate::local::sql::connections::SqlConnections;
use crate::local::sql::validation::require_non_empty;
use crate::{DbError, ProjectIdentity, Result};

pub(crate) struct ProjectLineageRepository<'db> {
    conn: &'db SqlConnections,
}

impl<'db> ProjectLineageRepository<'db> {
    pub(crate) fn new(conn: &'db SqlConnections) -> Self {
        Self { conn }
    }

    /// Returns the durable identity for `project_root`, creating it on first use.
    pub(crate) fn get_or_create(&self, project_root: &str) -> Result<ProjectIdentity> {
        let project_root = normalized_project_root(project_root)?;
        // Existing membership checks must not queue behind unrelated graph publication.
        if let Some(identity) = read_project_lineage(&self.conn.read_conn(), &project_root)? {
            return Ok(identity);
        }
        self.create_if_missing(&project_root)
    }

    fn create_if_missing(&self, project_root: &str) -> Result<ProjectIdentity> {
        let now = Utc::now().to_rfc3339();
        let candidate_project_id = Uuid::new_v4().to_string();
        let conn = self.conn.write_conn();
        conn.execute(
            "INSERT OR IGNORE INTO semantic_project_lineages
             (project_root, project_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![&project_root, candidate_project_id, now],
        )?;
        read_project_lineage(&conn, project_root)?
            .ok_or_else(|| DbError::invalid_value(project_root, "durable semantic project lineage"))
    }
}

fn read_project_lineage(
    conn: &rusqlite::Connection,
    root: &str,
) -> Result<Option<ProjectIdentity>> {
    let project_id = conn
        .query_row(
            "SELECT project_id FROM semantic_project_lineages WHERE project_root = ?1",
            [root],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    project_id.map(validated_project_identity).transpose()
}

fn validated_project_identity(project_id: String) -> Result<ProjectIdentity> {
    let identity = Uuid::parse_str(&project_id)
        .map_err(|_| DbError::invalid_value(&project_id, "UUID v4 semantic project identity"))?;
    if identity.get_version() != Some(Version::Random) {
        return Err(DbError::invalid_value(
            project_id,
            "UUID v4 semantic project identity",
        ));
    }
    Ok(ProjectIdentity { project_id })
}

pub(crate) fn restore_project_identity(
    conn: &rusqlite::Connection,
    root: &str,
    project_id: &str,
    empty_project: bool,
) -> Result<()> {
    let existing = conn
        .query_row(
            "SELECT project_id FROM semantic_project_lineages WHERE project_root = ?1",
            [root],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing.as_deref().is_some_and(|id| id != project_id) && !empty_project {
        return Err(DbError::invalid_value(
            project_id,
            format!(
                "the existing project lineage {} for {root}",
                existing.as_deref().unwrap_or_default()
            ),
        ));
    }
    let now = Utc::now().to_rfc3339();
    conn.execute("INSERT INTO semantic_project_lineages(project_root, project_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?3) ON CONFLICT(project_root) DO UPDATE SET project_id = excluded.project_id, updated_at = excluded.updated_at", params![root, project_id, now])?;
    Ok(())
}

fn normalized_project_root(project_root: &str) -> Result<String> {
    let normalized = project_root.trim().replace('\\', "/");
    require_non_empty(&normalized, "non-empty project root")?;
    if normalized.chars().all(|character| character == '/') {
        return Ok("/".to_owned());
    }
    Ok(normalized.trim_end_matches('/').to_owned())
}
