use super::*;
use std::{sync::mpsc::sync_channel, thread};

#[test]
fn catalog_observation_runs_after_installations_snapshot_unlocks() {
    let system = Arc::new(PluginSystem::new(PluginRuntimeConfig::default()));
    let (entered_tx, entered_rx) = sync_channel(0);
    let (release_tx, release_rx) = sync_channel(0);
    let worker_system = Arc::clone(&system);
    let worker = thread::spawn(move || {
        worker_system
            .with_catalog_entries(|entries| {
                entered_tx.send(()).expect("signal catalog observation");
                release_rx.recv().expect("release catalog observation");
                entries.len()
            })
            .expect("catalog snapshot")
    });

    entered_rx.recv().expect("catalog observation started");
    assert!(
        system.installations.try_lock().is_ok(),
        "installations mutex remained held during catalog observation"
    );
    release_tx.send(()).expect("release catalog observation");
    assert_eq!(worker.join().expect("catalog observer thread"), 0);
}
