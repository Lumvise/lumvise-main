use crate::local::clock::Clock;
use crate::local::sql::connections::SqlConnections;
use crate::local::sql::validation::require_non_empty;
use crate::{ArtifactBlob, DbError, Result};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ArtifactBlobRepository<'db> {
    conn: &'db SqlConnections,
    clock: Arc<dyn Clock>,
}

impl<'db> ArtifactBlobRepository<'db> {
    pub(crate) fn with_clock(conn: &'db SqlConnections, clock: Arc<dyn Clock>) -> Self {
        Self { conn, clock }
    }

    /// Stores artifact blob bytes by content reference.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::ArtifactBlobPut {
    ///         content_ref: "blob://note".into(),
    ///         artifact_id: "note".into(),
    ///         media_type: "text/plain".into(),
    ///         content: b"body".to_vec(),
    ///     },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::ArtifactBlob(Some(blob)) if blob.content.as_slice() == b"body"));
    /// ```
    pub fn put_blob(
        &self,
        content_ref: &str,
        artifact_id: &str,
        media_type: &str,
        content: &[u8],
    ) -> Result<()> {
        require_non_empty(content_ref, "non-empty blob content ref")?;
        require_non_empty(artifact_id, "non-empty artifact id")?;
        require_non_empty(media_type, "non-empty media type")?;
        let now = self.clock.now().to_rfc3339();
        put_valid_blob(
            &self.conn.write_conn(),
            content_ref,
            artifact_id,
            media_type,
            content,
            &now,
        )
    }

    /// Reads artifact blob bytes by content reference.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::{LocalPersistence, SemanticOperation, SemanticPersistence, SemanticResult};
    /// use lumvise_resource_routing::InvocationControl;
    ///
    /// let persistence = LocalPersistence::in_memory().unwrap();
    /// let control = InvocationControl::sixty_seconds();
    /// let result = SemanticPersistence::execute(
    ///     &persistence,
    ///     SemanticOperation::ArtifactBlobGet { content_ref: "blob://missing".into() },
    ///     &control,
    /// ).unwrap();
    /// assert!(matches!(result, SemanticResult::ArtifactBlob(None)));
    /// ```
    pub fn blob(&self, content_ref: &str) -> Result<Option<ArtifactBlob>> {
        require_non_empty(content_ref, "non-empty blob content ref")?;
        let conn = self.conn.read_conn();
        Ok(conn
            .query_row(
                "SELECT content_ref, artifact_id, media_type, content, updated_at
                 FROM artifact_blobs WHERE content_ref = ?1",
                params![content_ref],
                artifact_blob_from_row,
            )
            .optional()?)
    }

    pub(crate) fn blobs_for_content_refs(
        &self,
        content_refs: &[&str],
    ) -> Result<HashMap<String, ArtifactBlob>> {
        if content_refs.is_empty() {
            return Ok(HashMap::new());
        }
        let refs_json = serde_json::to_string(content_refs)?;
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare(
            "SELECT content_ref, artifact_id, media_type, content, updated_at
             FROM artifact_blobs WHERE content_ref IN (SELECT value FROM json_each(?1))",
        )?;
        let mut blobs = HashMap::with_capacity(content_refs.len());
        for row in statement.query_map(params![refs_json], artifact_blob_from_row)? {
            let blob = row?;
            blobs.insert(blob.content_ref.clone(), blob);
        }
        Ok(blobs)
    }

    /// Reads every blob owned by the given artifacts: content payloads and
    /// attachments such as canvas images.
    pub(crate) fn blobs_for_artifacts(&self, artifact_ids: &[&str]) -> Result<Vec<ArtifactBlob>> {
        if artifact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(artifact_ids)?;
        let conn = self.conn.read_conn();
        let mut statement = conn.prepare(
            "SELECT content_ref, artifact_id, media_type, content, updated_at
             FROM artifact_blobs WHERE artifact_id IN (SELECT value FROM json_each(?1))
             ORDER BY content_ref",
        )?;
        let blobs = statement
            .query_map(params![ids_json], artifact_blob_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(blobs)
    }

    /// Deletes every blob one artifact owns, returning how many were removed.
    pub(crate) fn delete_blobs_for_artifact(&self, artifact_id: &str) -> Result<usize> {
        require_non_empty(artifact_id, "non-empty artifact id")?;
        Ok(self.conn.write_conn().execute(
            "DELETE FROM artifact_blobs WHERE artifact_id = ?1",
            params![artifact_id],
        )?)
    }

    /// Deletes one artifact blob immediately.
    pub fn delete_blob(&self, content_ref: &str) -> Result<bool> {
        require_non_empty(content_ref, "non-empty blob content ref")?;
        let deleted = self.conn.write_conn().execute(
            "DELETE FROM artifact_blobs WHERE content_ref = ?1",
            params![content_ref],
        )?;
        Ok(deleted > 0)
    }
}

pub(crate) fn require_stored_blob_references<'a>(
    conn: &rusqlite::Connection,
    references: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut statement =
        conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM artifact_blobs WHERE content_ref = ?1)")?;
    for content_ref in references {
        if !statement.query_row([content_ref], |row| row.get::<_, bool>(0))? {
            return Err(DbError::invalid_value(
                content_ref,
                "stored exact artifact bytes inside the revision publication transaction",
            ));
        }
    }
    Ok(())
}

pub(crate) fn put_archived_blob(conn: &rusqlite::Connection, blob: &ArtifactBlob) -> Result<()> {
    require_non_empty(&blob.content_ref, "non-empty archived blob reference")?;
    require_non_empty(&blob.artifact_id, "non-empty archived blob owner")?;
    require_non_empty(&blob.media_type, "non-empty archived blob media type")?;
    put_valid_blob(
        conn,
        &blob.content_ref,
        &blob.artifact_id,
        &blob.media_type,
        &blob.content,
        &blob.updated_at,
    )
}

fn put_valid_blob(
    conn: &rusqlite::Connection,
    content_ref: &str,
    artifact_id: &str,
    media_type: &str,
    content: &[u8],
    now: &str,
) -> Result<()> {
    let inserted = conn.execute(
            "INSERT INTO artifact_blobs(content_ref, artifact_id, media_type, content, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5) ON CONFLICT(content_ref) DO NOTHING",
            params![content_ref, artifact_id, media_type, content, now],
        )?;
    if inserted == 1 {
        return Ok(());
    }
    let stored: (String, Vec<u8>) = conn.query_row(
        "SELECT media_type, content FROM artifact_blobs WHERE content_ref = ?1",
        [content_ref],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if stored.0 != media_type || stored.1 != content {
        return Err(DbError::invalid_value(
            content_ref,
            "identical bytes and media type for an immutable blob reference; allocate a new content ref for an edit",
        ));
    }
    Ok(())
}
fn artifact_blob_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactBlob> {
    Ok(ArtifactBlob {
        content_ref: row.get(0)?,
        artifact_id: row.get(1)?,
        media_type: row.get(2)?,
        content: row.get(3)?,
        updated_at: row.get(4)?,
    })
}
