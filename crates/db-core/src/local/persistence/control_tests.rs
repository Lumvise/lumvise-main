use super::{InvocationControl, LocalPersistence, SemanticOperation, SemanticPersistence};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn cancelled_sync_waiter_exits_while_other_sync_holds_lane() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let storage = persistence.core.storage_manager().semantic_storage();
    let owner = InvocationControl::sixty_seconds();
    let lane = storage.graph.lock_structure_sync(&owner).unwrap();
    let caller = InvocationControl::sixty_seconds();
    let (started_tx, started_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let caller = &caller;
        let persistence = &persistence;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result = persistence.execute(
                SemanticOperation::SyncStructure {
                    project_root: "/cancelled".into(),
                    elements: vec![],
                    relationships: vec![],
                },
                caller,
            );
            finished_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(30)).is_err());
        caller.cancel();
        let result = finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cancelled invocation")
        );
    });
    drop(lane);
    assert!(
        persistence
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: "/next".into(),
                    elements: vec![],
                    relationships: vec![],
                },
                &InvocationControl::sixty_seconds()
            )
            .is_ok()
    );
}

#[test]
fn expired_sync_waiter_does_not_start_a_second_plan() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let storage = persistence.core.storage_manager().semantic_storage();
    let owner = InvocationControl::sixty_seconds();
    let _lane = storage.graph.lock_structure_sync(&owner).unwrap();
    let caller = InvocationControl::with_deadline(Duration::from_millis(25));
    let result = persistence.execute(
        SemanticOperation::SyncPartition {
            partition: crate::SemanticPartition {
                project_root: "/expired".into(),
                replace_paths: vec!["file.rs".into()],
            },
            elements: vec![],
            relationships: vec![],
        },
        &caller,
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("expired invocation")
    );
}
