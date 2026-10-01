use lumvise_db_core::{DbCore, PluginBackgroundRegistrationSpec};
use serde_json::json;

fn recurring_registration(next_due_at: i64) -> PluginBackgroundRegistrationSpec {
    PluginBackgroundRegistrationSpec {
        plugin_id: "builtin.nucleus".into(),
        export_id: "scan_reports".into(),
        export_kind: "recurring_task".into(),
        contract: json!({"interval_seconds": 600}),
        next_due_at: Some(next_due_at),
    }
}

fn change_hook_registration() -> PluginBackgroundRegistrationSpec {
    PluginBackgroundRegistrationSpec {
        plugin_id: "builtin.knowledge".into(),
        export_id: "semantic_changed".into(),
        export_kind: "change_hook".into(),
        contract: json!({"event_kinds": ["semantic.element.upserted"]}),
        next_due_at: None,
    }
}

#[test]
fn registration_sync_is_idempotent_and_deactivates_absent_exports() {
    let db = DbCore::in_memory().expect("database");
    let repository = db.plugin_deliveries();
    repository
        .sync_registrations(&[recurring_registration(10), change_hook_registration()])
        .expect("initial sync");
    repository
        .sync_registrations(&[recurring_registration(99)])
        .expect("repeat sync");

    let registrations = repository.active_registrations().expect("registrations");

    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].next_due_at, Some(10));
}

#[test]
fn registration_sync_rejects_unbounded_catalogs() {
    let db = DbCore::in_memory().expect("database");
    let registrations = (0..257)
        .map(|index| PluginBackgroundRegistrationSpec {
            plugin_id: "compiled.plugin".into(),
            export_id: format!("background-{index}"),
            export_kind: "change_hook".into(),
            contract: json!({"event_kinds": ["semantic.element.upserted"]}),
            next_due_at: None,
        })
        .collect::<Vec<_>>();

    let result = db.plugin_deliveries().sync_registrations(&registrations);

    assert!(result.is_err());
    assert!(
        db.plugin_deliveries()
            .active_registrations()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recurring_slot_is_enqueued_once_and_advances_durable_cursor() {
    let db = DbCore::in_memory().expect("database");
    let repository = db.plugin_deliveries();
    repository
        .sync_registrations(&[recurring_registration(10)])
        .expect("registration");

    repository
        .enqueue_recurring("builtin.nucleus", "scan_reports", 10, 10, 600, &json!({}))
        .expect("first enqueue");
    repository
        .enqueue_recurring("builtin.nucleus", "scan_reports", 10, 10, 600, &json!({}))
        .expect("duplicate enqueue");

    let due = repository.due_deliveries(10, 10).expect("due");
    assert_eq!(due.len(), 1);
    assert_eq!(
        repository.active_registrations().unwrap()[0].next_due_at,
        Some(610)
    );
}

#[test]
fn recurring_slot_coalesces_missed_cadences_after_downtime() {
    let db = DbCore::in_memory().expect("database");
    let repository = db.plugin_deliveries();
    repository
        .sync_registrations(&[recurring_registration(10)])
        .expect("registration");

    repository
        .enqueue_recurring(
            "builtin.nucleus",
            "scan_reports",
            10,
            1_900,
            600,
            &json!({}),
        )
        .expect("coalesced enqueue");

    assert_eq!(
        repository.active_registrations().unwrap()[0].next_due_at,
        Some(2_410)
    );
}
