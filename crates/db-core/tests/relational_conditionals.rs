//! Portable row preconditions protect pending publications and scoped acknowledgements.
mod common;
use common::LocalPublicationServer;
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};

const OWNER: &str = "app.index-publication";
const TABLE: &str = "checkpoints";

fn execute(
    store: &dyn RelationalPersistence,
    operation: RelationalOperation,
) -> PersistenceResult<RelationalResult> {
    store.execute(operation, &InvocationControl::sixty_seconds())
}

fn prepare(store: &dyn RelationalPersistence) {
    execute(
        store,
        RelationalOperation::EnsurePluginDataTable {
            plugin_id: OWNER.into(),
            table_name: TABLE.into(),
            schema: json!({}),
        },
    )
    .unwrap();
}

fn put(key: &str, value: Value) -> PluginDataMutation {
    PluginDataMutation::Put {
        table_name: TABLE.into(),
        row_key: key.into(),
        value,
    }
}

fn require(key: &str, expected: Option<Value>) -> PluginDataMutation {
    PluginDataMutation::RequireValue {
        table_name: TABLE.into(),
        row_key: key.into(),
        expected: expected.map_or(
            PluginDataExpectedValue::Absent,
            PluginDataExpectedValue::Present,
        ),
    }
}

fn apply(
    store: &dyn RelationalPersistence,
    mutations: Vec<PluginDataMutation>,
) -> PersistenceResult<RelationalResult> {
    execute(
        store,
        RelationalOperation::ApplyMutations {
            plugin_id: OWNER.into(),
            mutations,
        },
    )
}

fn value(store: &dyn RelationalPersistence, key: &str) -> Option<Value> {
    let result = execute(
        store,
        RelationalOperation::GetPluginData {
            plugin_id: OWNER.into(),
            table_name: TABLE.into(),
            row_key: key.into(),
        },
    )
    .unwrap();
    let RelationalResult::PluginDataRow(row) = result else {
        panic!("expected plugin row")
    };
    row.map(|row| row.value)
}

fn port(remote: bool) -> Arc<dyn RelationalPersistence> {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    if !remote {
        return local;
    }
    Arc::new(CentralizedPersistence::new(
        Arc::new(LocalPublicationServer(local)),
        "checkout-a",
    ))
}

fn page(
    store: &dyn RelationalPersistence,
    prefix: &str,
    after: Option<String>,
    limit: usize,
) -> PluginDataRowsPage {
    match execute(
        store,
        RelationalOperation::PagePluginData {
            plugin_id: OWNER.into(),
            table_name: TABLE.into(),
            key_prefix: Some(prefix.into()),
            after_key: after,
            limit,
        },
    )
    .unwrap()
    {
        RelationalResult::PluginDataPage(page) => page,
        other => panic!("{other:?}; expected one literal-prefix row page"),
    }
}

#[test]
fn row_prefixes_preserve_unicode_nul_separators_and_literal_wildcards_through_both_ports() {
    for remote in [false, true] {
        let store = port(remote);
        prepare(&*store);
        let keys = ["π%_\0a", "π%_\0b", "π%_wrong", "π-other", "other\0a"];
        apply(
            &*store,
            keys.into_iter()
                .map(|key| put(key, json!({"key":key})))
                .collect(),
        )
        .unwrap();
        let first = page(&*store, "π%_\0", None, 1);
        assert_eq!(
            first.rows.len(),
            1,
            "literal NUL prefix through remote={remote}"
        );
        assert_eq!(first.rows[0].row_key, "π%_\0a");
        let next = first.next_after_key.expect("second matching row remains");
        let second = page(&*store, "π%_\0", Some(next), 1);
        assert_eq!(second.rows[0].row_key, "π%_\0b");
        assert!(second.next_after_key.is_none());
        assert_eq!(page(&*store, "π", None, 100).rows.len(), 4);
    }
}

#[test]
fn rejected_checkpoint_batch_preserves_every_row_through_both_ports() {
    for remote in [false, true] {
        let store = port(remote);
        prepare(&*store);
        apply(
            &*store,
            vec![
                require("pending", None),
                put("pending", json!({"operation":"one"})),
            ],
        )
        .unwrap();
        let failed = apply(
            &*store,
            vec![put("file", json!("wrong")), require("pending", None)],
        );
        assert!(failed.is_err());
        assert_eq!(value(&*store, "pending"), Some(json!({"operation":"one"})));
        assert_eq!(value(&*store, "file"), None);
        let outcome = apply(
            &*store,
            vec![
                require("pending", Some(json!({"operation":"one"}))),
                put("file", json!({"accepted":"e23"})),
                PluginDataMutation::Delete {
                    table_name: TABLE.into(),
                    row_key: "pending".into(),
                },
            ],
        )
        .unwrap();
        assert!(
            matches!(outcome, RelationalResult::PluginDataMutations(counts) if counts.rows_put == 1 && counts.rows_deleted == 1)
        );
        assert_eq!(value(&*store, "pending"), None);
        assert_eq!(value(&*store, "file"), Some(json!({"accepted":"e23"})));
    }
}

#[test]
fn requirements_observe_the_original_batch_and_distinguish_absence_from_null() {
    for remote in [false, true] {
        let store = port(remote);
        prepare(&*store);
        apply(
            &*store,
            vec![put("pending", Value::Null), require("pending", None)],
        )
        .unwrap();
        assert!(apply(&*store, vec![require("pending", None)]).is_err());
        let checked = apply(&*store, vec![require("pending", Some(Value::Null))]).unwrap();
        assert!(
            matches!(checked, RelationalResult::PluginDataMutations(counts) if counts.rows_put == 0 && counts.rows_deleted == 0)
        );
    }
}

#[test]
fn concurrent_pending_requests_have_only_one_winner() {
    for remote in [false, true] {
        let store = port(remote);
        prepare(&*store);
        let gate = Arc::new(Barrier::new(2));
        let workers = (0..2)
            .map(|id| {
                let (store, gate) = (Arc::clone(&store), Arc::clone(&gate));
                std::thread::spawn(move || {
                    gate.wait();
                    apply(
                        &*store,
                        vec![
                            require("pending", None),
                            put("pending", json!({"operation": id})),
                        ],
                    )
                    .is_ok()
                })
            })
            .collect::<Vec<_>>();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "remote={remote}");
        assert!(value(&*store, "pending").is_some());
    }
}
