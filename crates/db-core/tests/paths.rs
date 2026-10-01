use lumvise_db_core::LocalPersistence;
use tempfile::TempDir;

#[test]
fn db_open_creates_missing_database_parent_directory() {
    let temp = TempDir::new().unwrap();
    let path = temp
        .path()
        .join("nested")
        .join("database")
        .join("lumvise.db");

    LocalPersistence::open(&path).unwrap();

    assert!(path.exists());
}
