#![cfg(debug_assertions)]

use lumvise_db_core::DbCore;
use serde_json::json;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Barrier, mpsc};
use std::thread::{self, JoinHandle};

#[test]
fn persistent_sql_read_uses_reader_pool_while_writer_connection_is_held() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = persistent_settings_db(temp.path());
    db.persistent_settings()
        .set_json("app", "theme", &json!("dark"))
        .unwrap();
    let (release_tx, writer) = hold_writer_connection(Arc::clone(&db));
    let (read_rx, reader) = spawn_setting_read(db);

    let setting = read_rx
        .recv()
        .expect("SQL read should use the independent reader pool");
    assert_eq!(setting.unwrap().value, json!("dark"));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    reader.join().unwrap();
}

#[test]
fn concurrent_persistent_sql_writes_commit_both_keys() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = persistent_settings_db(temp.path());
    let start = Arc::new(Barrier::new(3));
    let first = spawn_setting_write(Arc::clone(&db), Arc::clone(&start), "theme", "dark");
    let second = spawn_setting_write(Arc::clone(&db), Arc::clone(&start), "locale", "de");

    start.wait();
    first.join().unwrap();
    second.join().unwrap();

    assert_eq!(setting_value(&db, "theme"), json!("dark"));
    assert_eq!(setting_value(&db, "locale"), json!("de"));
}

fn persistent_settings_db(path: &std::path::Path) -> Arc<DbCore> {
    Arc::new(DbCore::open(path).unwrap())
}

fn hold_writer_connection(db: Arc<DbCore>) -> (Sender<()>, JoinHandle<()>) {
    let (release_tx, release_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        db.hold_sql_writer_for_test(|| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
    });
    entered_rx.recv().unwrap();
    (release_tx, writer)
}

fn spawn_setting_read(
    db: Arc<DbCore>,
) -> (
    mpsc::Receiver<Option<lumvise_db_core::SettingRecord>>,
    JoinHandle<()>,
) {
    let (read_tx, read_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let setting = db.persistent_settings().get_json("app", "theme").unwrap();
        read_tx.send(setting).unwrap();
    });
    (read_rx, reader)
}

fn spawn_setting_write(
    db: Arc<DbCore>,
    start: Arc<Barrier>,
    key: &'static str,
    value: &'static str,
) -> JoinHandle<()> {
    thread::spawn(move || {
        start.wait();
        db.persistent_settings()
            .set_json("app", key, &json!(value))
            .unwrap();
    })
}

fn setting_value(db: &DbCore, key: &str) -> serde_json::Value {
    db.persistent_settings()
        .get_json("app", key)
        .unwrap()
        .unwrap()
        .value
}
