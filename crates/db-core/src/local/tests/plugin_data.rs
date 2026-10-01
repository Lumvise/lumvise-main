use lumvise_db_core::DbCore;
use serde_json::json;

#[test]
fn plugin_data_repository_stores_plugin_scoped_rows() {
    let db = DbCore::in_memory().unwrap();
    let repo = db.plugin_data();

    repo.ensure_table(
        "builtin.assistant",
        "session_cache",
        &json!({ "type": "object" }),
    )
    .unwrap();
    repo.put_row(
        "builtin.assistant",
        "session_cache",
        "assistant-1",
        &json!({ "phase": "listening" }),
    )
    .unwrap();

    let row = repo
        .row("builtin.assistant", "session_cache", "assistant-1")
        .unwrap()
        .unwrap();

    assert_eq!(row.value["phase"], "listening");
    assert_eq!(
        repo.rows("builtin.assistant", "session_cache")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn plugin_data_rows_require_declared_logical_table() {
    let db = DbCore::in_memory().unwrap();

    let error = db
        .plugin_data()
        .put_row("builtin.assistant", "missing", "key", &json!({}))
        .unwrap_err();

    assert!(error.to_string().contains("FOREIGN KEY"));
}

#[test]
fn plugin_data_trim_retains_newest_row_keys() {
    let db = lumvise_db_core::DbCore::in_memory().unwrap();
    let rows = db.plugin_data();
    rows.ensure_table("builtin.knowledge", "events", &serde_json::json!({}))
        .unwrap();
    for key in ["0001", "0002", "0003"] {
        rows.put_row(
            "builtin.knowledge",
            "events",
            key,
            &serde_json::json!({"key": key}),
        )
        .unwrap();
    }
    assert_eq!(
        rows.trim_rows_by_key("builtin.knowledge", "events", 2)
            .unwrap(),
        1
    );
    let keys = rows
        .rows("builtin.knowledge", "events")
        .unwrap()
        .into_iter()
        .map(|row| row.row_key)
        .collect::<Vec<_>>();
    assert_eq!(keys, ["0002", "0003"]);
}
