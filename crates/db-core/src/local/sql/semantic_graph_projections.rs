use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, SemanticGraphProjection};

pub(crate) struct SemanticGraphProjectionRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> SemanticGraphProjectionRepository<'connection> {
    pub(crate) fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub(crate) fn get(
        &self,
        project_root: &str,
        commit_version: i64,
    ) -> Result<Option<SemanticGraphProjection>> {
        let bytes = self
            .connection
            .query_row(
                "SELECT projection_json FROM semantic_graph_projections \
                 WHERE project_root = ?1 AND commit_version = ?2",
                params![project_root, commit_version],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        bytes
            .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn put(&self, projection: &SemanticGraphProjection) -> Result<()> {
        let bytes = serde_json::to_vec(projection)?;
        self.connection.execute(
            "INSERT INTO semantic_graph_projections \
             (project_root, commit_version, projection_json, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(project_root) DO UPDATE SET \
             commit_version = excluded.commit_version, \
             projection_json = excluded.projection_json, updated_at = excluded.updated_at",
            params![
                projection.project_root,
                projection.commit_version,
                bytes,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}
