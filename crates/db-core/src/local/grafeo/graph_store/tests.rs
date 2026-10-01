use super::GraphStore;
use crate::{
    DbError, Result, SemanticGraphGranularity, SemanticGraphProjection,
    SemanticGraphProjectionRequest,
};
use grafeo::GrafeoDB;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

#[test]
fn automatic_checkpoint_allows_published_reads_with_a_waiting_writer() {
    let store = Arc::new(GraphStore::new(GrafeoDB::new_in_memory()));
    let (entered, checkpoint_started) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    *store.work.checkpoint_probe.lock().unwrap() = Some(super::CheckpointProbe {
        entered,
        release: resume,
    });
    let publishing_store = Arc::clone(&store);
    let publisher = thread::spawn(move || {
        publishing_store.write_semantic_through_post_commit(
            |graph| {
                graph.create_node_with_props(&["Published"], [])?;
                Ok(())
            },
            |_| Ok(()),
            |_| super::SEMANTIC_ENTITIES_PER_CHECKPOINT,
        )
    });
    checkpoint_started
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let writing_store = Arc::clone(&store);
    let (written, write_completed) = mpsc::channel();
    let writer = thread::spawn(move || {
        let result = writing_store.write(|graph| graph.create_node_with_props(&["Later"], []));
        written.send(()).unwrap();
        result
    });
    let writer_waited = write_completed
        .recv_timeout(Duration::from_millis(30))
        .is_err();
    let reading_store = Arc::clone(&store);
    let (read, read_completed) = mpsc::channel();
    let reader = thread::spawn(move || {
        read.send(reading_store.stable_read(|graph| Ok(graph.iter_nodes().count())))
            .unwrap();
    });
    let observed = read_completed.recv_timeout(Duration::from_secs(2));
    release.send(()).unwrap();
    publisher.join().unwrap().unwrap();
    writer.join().unwrap().unwrap();
    reader.join().unwrap();
    assert!(writer_waited, "checkpoint must retain writer exclusion");
    assert_eq!(
        observed.unwrap().unwrap(),
        1,
        "read must see the published graph during checkpoint"
    );
    assert_eq!(
        store
            .stable_read(|graph| Ok(graph.iter_nodes().count()))
            .unwrap(),
        2
    );
}

#[test]
fn failed_post_commit_invalidates_projection_cache_and_blocks_stable_reads() {
    let store = GraphStore::new(GrafeoDB::new_in_memory());
    let request = SemanticGraphProjectionRequest {
        project_root: "/repo".into(),
        target_path: None,
        granularity: SemanticGraphGranularity::File,
        recursive: true,
        include_external: false,
        include_first_neighbors: false,
    };
    store.cache_projection(
        0,
        request,
        SemanticGraphProjection {
            commit_version: 0,
            published_at: "initial".into(),
            project_root: "/repo".into(),
            nodes: Vec::new(),
            edges: Vec::new(),
        },
    );

    let result = store
        .write_semantic_through_post_commit(
            |graph| {
                graph.create_node_with_props(&["Mutated"], [])?;
                Ok(())
            },
            |_| Err::<(), _>(DbError::Grafeo("forced SQL publication failure".into())),
            |_| 1,
        )
        .unwrap();
    match result {
        super::GraphPostCommit::Failed(error) => {
            assert!(error.to_string().contains("forced SQL publication failure"));
        }
        super::GraphPostCommit::Completed(_) => panic!("publication unexpectedly succeeded"),
    }
    assert_eq!(store.projection_cache_len(), 0);
    assert_eq!(store.read(|graph| graph.iter_nodes().count()), 1);
    let read_error = store.stable_read(|_| Ok(())).unwrap_err();
    assert!(
        read_error
            .to_string()
            .contains("forced SQL publication failure")
    );

    let result = store
        .write_semantic_through_post_commit(
            |graph| {
                graph.create_node_with_props(&["Recovered"], [])?;
                Ok(())
            },
            |_| Ok(()),
            |_| 0,
        )
        .unwrap();
    assert!(matches!(result, super::GraphPostCommit::Completed(())));
    store.stable_read(|_| Ok(())).unwrap();
}

#[test]
fn failed_graph_transaction_rolls_back_all_changes() {
    let store = GraphStore::new(GrafeoDB::new_in_memory());
    let result = write_then_fail(&store);
    assert!(result.is_err());
    assert_eq!(store.read(|graph| graph.iter_nodes().count()), 0);
    assert!(rolled_back_property_index_is_empty(&store));
}

#[test]
fn leased_snapshot_does_not_observe_uncommitted_graph_changes() {
    let store = Arc::new(GraphStore::new(GrafeoDB::new_in_memory()));
    let snapshot = store.snapshot();
    let (changed_rx, release_tx, writer) = spawn_pending_writer(Arc::clone(&store));
    changed_rx.recv().unwrap();
    let visible_before_commit = snapshot.iter_nodes().count();
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    assert_eq!(visible_before_commit, 0);
    assert_eq!(store.read(|graph| graph.iter_nodes().count()), 1);
}

#[test]
fn persistent_graph_writers_enter_mutation_closures_serially() {
    let (_directory, store) = persistent_graph_store();
    let (first_entered_rx, first_release_tx, first) =
        spawn_held_writer(Arc::clone(&store), "First");
    first_entered_rx.recv().unwrap();
    let (second_entered_rx, second_release_tx, second) =
        spawn_held_writer(Arc::clone(&store), "Second");
    let overlapped = second_entered_rx
        .recv_timeout(Duration::from_millis(100))
        .is_ok();
    first_release_tx.send(()).unwrap();
    await_second_writer(overlapped, &second_entered_rx);
    second_release_tx.send(()).unwrap();
    first.join().unwrap();
    second.join().unwrap();
    assert!(!overlapped, "graph mutation closures must not overlap");
}

#[test]
fn stable_read_callbacks_can_enter_concurrently() {
    let store = Arc::new(GraphStore::new(GrafeoDB::new_in_memory()));
    let (first_entered_rx, first_release_tx, first) = spawn_held_stable_read(Arc::clone(&store));
    let first_entered = first_entered_rx
        .recv_timeout(Duration::from_secs(1))
        .is_ok();

    let (second_entered_rx, second_release_tx, second) = spawn_held_stable_read(Arc::clone(&store));
    let second_entered_before_first_release = first_entered
        && second_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .is_ok();

    first_release_tx.send(()).unwrap();
    if !second_entered_before_first_release {
        let _ = second_entered_rx.recv_timeout(Duration::from_secs(1));
    }
    second_release_tx.send(()).unwrap();
    first.join().unwrap();
    second.join().unwrap();

    assert!(first_entered, "first stable read callback did not enter");
    assert!(
        second_entered_before_first_release,
        "stable read callbacks must overlap before either is released"
    );
}

#[test]
fn stale_plan_retries_without_losing_the_intervening_write() {
    let store = GraphStore::new(GrafeoDB::new_in_memory());
    let initial = store.plan(|_| Ok(())).unwrap();
    store
        .write(|graph| {
            graph.create_node_with_props(&["Intervening"], [])?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        store
            .write_if_revision(initial.into_parts().0, |_| Ok(()))
            .unwrap(),
        super::ConditionalGraphWrite::Stale
    ));

    let (revision, ()) = store.plan(|_| Ok(())).unwrap().into_parts();
    store
        .write_if_revision(revision, |graph| {
            graph.create_node_with_props(&["Retried"], [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.read(|graph| graph.iter_nodes().count()), 2);
}

#[test]
fn planner_does_not_hold_the_writer_gate() {
    let store = Arc::new(GraphStore::new(GrafeoDB::new_in_memory()));
    let (planner_started_tx, planner_started_rx) = mpsc::channel();
    let (release_planner_tx, release_planner_rx) = mpsc::channel();
    let pause_first_plan = Arc::new(AtomicBool::new(true));
    let planner_store = Arc::clone(&store);
    let planner = thread::spawn(move || {
        let revision = planner_store
            .plan(|_| {
                if pause_first_plan.swap(false, Ordering::AcqRel) {
                    planner_started_tx.send(()).unwrap();
                    release_planner_rx.recv().unwrap();
                }
                Ok(())
            })
            .unwrap()
            .into_parts()
            .0;
        assert_eq!(
            revision, 2,
            "the planner retries after the intervening writer advances the revision"
        );
    });
    planner_started_rx.recv().unwrap();

    let (writer_done_tx, writer_done_rx) = mpsc::channel();
    let writer_store = Arc::clone(&store);
    let writer = thread::spawn(move || {
        writer_store
            .write(|graph| {
                graph.create_node_with_props(&["Independent"], [])?;
                Ok(())
            })
            .unwrap();
        writer_done_tx.send(()).unwrap();
    });
    writer_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("independent writer commits while planner is paused outside the writer gate");
    release_planner_tx.send(()).unwrap();
    planner.join().unwrap();
    writer.join().unwrap();
}

fn write_then_fail(store: &GraphStore) -> Result<()> {
    store.write(|graph| {
        graph.create_node_with_props(
            &["Uncommitted"],
            [("kind", grafeo::Value::from("uncommitted"))],
        )?;
        Err(DbError::Grafeo("forced transaction failure".to_string()))
    })
}

fn rolled_back_property_index_is_empty(store: &GraphStore) -> bool {
    store.read(|graph| {
        graph
            .find_nodes_by_property("kind", &grafeo::Value::from("uncommitted"))
            .is_empty()
    })
}

fn spawn_pending_writer(
    store: Arc<GraphStore>,
) -> (mpsc::Receiver<()>, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (changed_tx, changed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        store
            .write(|graph| {
                graph.create_node_with_props(&["Pending"], [])?;
                changed_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    (changed_rx, release_tx, writer)
}

fn persistent_graph_store() -> (tempfile::TempDir, Arc<GraphStore>) {
    let directory = tempfile::tempdir().unwrap();
    let graph_path = directory.path().join("atomic.grafeo");
    let graph = GrafeoDB::open(graph_path).unwrap();
    (directory, Arc::new(GraphStore::new(graph)))
}

fn spawn_held_writer(
    store: Arc<GraphStore>,
    label: &'static str,
) -> (mpsc::Receiver<()>, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        store
            .write(|graph| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                graph.create_node_with_props(&[label], [])?;
                Ok(())
            })
            .unwrap();
    });
    (entered_rx, release_tx, writer)
}

fn spawn_held_stable_read(
    store: Arc<GraphStore>,
) -> (mpsc::Receiver<()>, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        store
            .stable_read(|_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    (entered_rx, release_tx, reader)
}

fn await_second_writer(overlapped: bool, entered: &mpsc::Receiver<()>) {
    if !overlapped {
        entered.recv().unwrap();
    }
}

/// T3.4 AC: a `wal_checkpoint` (`GraphStore::checkpoint`) run after a write
/// must not corrupt the graph — the written state survives a reopen, so the
/// periodic + at-open + on-shutdown checkpoint keeps on-disk state consistent
/// and bounded across sessions.
#[test]
fn checkpoint_persists_state_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let graph_path = directory.path().join("maintenance.grafeo");

    {
        let store = GraphStore::new(GrafeoDB::open(&graph_path).unwrap());
        store
            .write(|graph| {
                graph.create_node_with_props(&["SemanticElement"], [])?;
                Ok(())
            })
            .unwrap();
        store
            .checkpoint()
            .expect("wal_checkpoint succeeds on populated graph");
    }

    let reopened = GraphStore::new(GrafeoDB::open(&graph_path).unwrap());
    let surviving = reopened.read(|graph| graph.iter_nodes().count());
    assert_eq!(surviving, 1, "written node survives checkpoint and reopen");
}

#[test]
fn thousand_logical_entities_checkpoint_once_into_main_graph_file() {
    let directory = tempfile::tempdir().unwrap();
    let graph_path = directory.path().join("cadence.grafeo");
    let store = GraphStore::new(GrafeoDB::open(&graph_path).unwrap());

    commit_semantic_entities(&store, 999, "BeforeThreshold");
    assert_eq!(store.committed_semantic_entities(), 999);
    commit_semantic_entities(&store, 1, "AtThreshold");
    assert_eq!(store.committed_semantic_entities(), 0);

    let checkpoint_copy = directory.path().join("checkpoint-copy.grafeo");
    std::fs::copy(&graph_path, &checkpoint_copy).unwrap();
    let reopened = GrafeoDB::open(checkpoint_copy).unwrap();
    assert_eq!(
        reopened.iter_nodes().count(),
        2,
        "the threshold checkpoint writes committed nodes into the main container"
    );
}

fn commit_semantic_entities(store: &GraphStore, count: usize, label: &str) {
    let outcome = store
        .write_semantic_through_post_commit(
            |graph| {
                graph.create_node_with_props(&[label], [])?;
                Ok(())
            },
            |()| Ok(count),
            |changed| *changed,
        )
        .unwrap();
    assert!(matches!(outcome, super::GraphPostCommit::Completed(_)));
}
