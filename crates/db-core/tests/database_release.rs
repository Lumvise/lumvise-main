use lumvise_db_core::{DbError, LocalPersistence};
use rusqlite::Connection;

#[test]
fn fresh_database_records_a_release_that_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lumvise.db");
    drop(LocalPersistence::open(&path).unwrap());
    let original = std::fs::read(&path).unwrap();
    drop(LocalPersistence::open(&path).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let conn = Connection::open(path).unwrap();
    let release: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(release, 1);
}

#[test]
fn unversioned_database_is_refused_without_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lumvise.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE previous_content(body TEXT); INSERT INTO previous_content VALUES ('keep me');").unwrap();
    drop(conn);
    let original = std::fs::read(&path).unwrap();
    assert_incompatible(LocalPersistence::open(&path), 0);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!directory.path().join("lumvise.db.semantic.grafeo").exists());
}

#[test]
fn newer_release_is_refused_before_graph_open_or_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lumvise.db");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let original = std::fs::read(&path).unwrap();
    assert_incompatible(LocalPersistence::open(&path), 2);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!directory.path().join("lumvise.db.semantic.grafeo").exists());
}

#[test]
fn empty_sqlite_with_an_existing_graph_is_not_a_fresh_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lumvise.db");
    let graph = directory.path().join("lumvise.db.semantic.grafeo");
    std::fs::write(&graph, b"existing graph state").unwrap();
    assert_incompatible(LocalPersistence::open(&path), 0);
    let conn = Connection::open(&path).unwrap();
    let release: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(release, 0);
    assert_eq!(std::fs::read(graph).unwrap(), b"existing graph state");
}

fn assert_incompatible(result: lumvise_db_core::Result<LocalPersistence>, stored: i64) {
    let error = match result {
        Ok(_) => panic!("database release {stored} opened; expected incompatible release error"),
        Err(error) => error,
    };
    assert!(
        matches!(error, DbError::UnsupportedDatabaseRelease { stored: actual, required: 1 } if actual == stored)
    );
    let message = error.to_string();
    assert!(message.contains(&format!("release {stored}")));
    assert!(message.contains("expected 1"));
    assert!(message.contains("back up and explicitly rebuild"));
}
