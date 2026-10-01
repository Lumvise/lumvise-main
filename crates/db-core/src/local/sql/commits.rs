use crate::{DbError, Result, SemanticRecoveryStatus};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn latest_published_commit_version(conn: &Connection) -> Result<i64> {
    let mut statement = conn.prepare_cached(
        "SELECT COALESCE(MAX(commit_version), 0) FROM semantic_commits WHERE state = 'published'",
    )?;
    let version = statement.query_row([], |row| row.get(0))?;
    Ok(version)
}

pub(crate) fn published_commit_timestamp(conn: &Connection, commit_version: i64) -> Result<String> {
    conn.query_row(
        "SELECT published_at FROM semantic_commits
         WHERE commit_version = ?1 AND state = 'published'",
        [commit_version],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn semantic_recovery_status(
    conn: &Connection,
) -> Result<Option<SemanticRecoveryStatus>> {
    let status = conn
        .query_row(
            "SELECT commit_version, state, failure_message FROM semantic_commits
             WHERE state IN ('pending', 'failed') ORDER BY commit_version LIMIT 1",
            [],
            |row| {
                Ok(SemanticRecoveryStatus {
                    commit_version: row.get(0)?,
                    state: row.get(1)?,
                    failure_message: row.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(status)
}

pub(crate) fn recover_stale_pending_commits(conn: &Connection) -> Result<usize> {
    let pending_versions = pending_commit_versions(conn)?;
    for commit_version in &pending_versions {
        publish_semantic_commit(conn, *commit_version)?;
    }
    Ok(pending_versions.len())
}

/// Resolves semantic commits stuck in `failed` state.
///
/// A failed commit means the graph transaction was rolled back atomically
/// (the write never landed), but the SQL commit record blocks all new
/// writes via `reject_unresolved_commit`. This marks such commits as
pub(crate) fn resolve_failed_commits(conn: &Connection) -> Result<usize> {
    let failed_versions = failed_commit_versions(conn)?;
    for commit_version in &failed_versions {
        conn.execute(
            "UPDATE semantic_commits SET state = 'resolved'
             WHERE commit_version = ?1 AND state = 'failed'",
            [commit_version],
        )?;
    }
    Ok(failed_versions.len())
}

fn failed_commit_versions(conn: &Connection) -> Result<Vec<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT commit_version FROM semantic_commits WHERE state = 'failed' ORDER BY commit_version",
    )?;
    let versions = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(versions)
}

fn pending_commit_versions(conn: &Connection) -> Result<Vec<i64>> {
    let mut statement = conn.prepare_cached(
        "SELECT commit_version FROM semantic_commits WHERE state = 'pending' ORDER BY commit_version",
    )?;
    let versions = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(versions)
}

pub(crate) fn begin_semantic_commit(conn: &Connection) -> Result<i64> {
    reject_unresolved_commit(conn)?;
    let next = latest_commit_version(conn)? + 1;
    let mut statement = conn.prepare_cached(
        "INSERT INTO semantic_commits(commit_version, state, created_at)
         VALUES (?1, 'pending', ?2)",
    )?;
    statement.execute(params![next, Utc::now().to_rfc3339()])?;
    Ok(next)
}

pub(crate) fn publish_semantic_commit(conn: &Connection, commit_version: i64) -> Result<()> {
    let mut statement = conn.prepare_cached(
        "UPDATE semantic_commits SET state = 'published', published_at = ?1
         WHERE commit_version = ?2 AND state = 'pending'",
    )?;
    statement.execute(params![Utc::now().to_rfc3339(), commit_version])?;
    Ok(())
}

pub(crate) fn fail_semantic_commit(
    conn: &Connection,
    commit_version: i64,
    failure_message: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE semantic_commits SET state = 'failed', failure_message = ?1
         WHERE commit_version = ?2 AND state = 'pending'",
        params![failure_message, commit_version],
    )?;
    Ok(())
}

fn reject_unresolved_commit(conn: &Connection) -> Result<()> {
    let unresolved = conn
        .query_row(
            "SELECT commit_version, state FROM semantic_commits
             WHERE state IN ('pending', 'failed') ORDER BY commit_version LIMIT 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match unresolved {
        Some((commit_version, state)) => Err(DbError::unresolved_commit(commit_version, state)),
        None => Ok(()),
    }
}

fn latest_commit_version(conn: &Connection) -> Result<i64> {
    let version = conn.query_row(
        "SELECT COALESCE(MAX(commit_version), 0) FROM semantic_commits",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}
