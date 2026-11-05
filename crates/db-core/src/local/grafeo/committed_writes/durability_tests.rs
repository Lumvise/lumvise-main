use crate::local::grafeo::graph_reads::CommittedGraph;
use crate::{DbError, LocalPersistence, SemanticPersistence};
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn graph_mutation_starts_with_a_durable_unresolved_publication() {
    let directory = tempfile::tempdir().unwrap();
    let local = LocalPersistence::open(directory.path().join("publication.db")).unwrap();
    let ready_during_write = AtomicBool::new(true);
    local
        .core
        .storage_manager()
        .semantic_storage()
        .commit_graph_write(|graph, _, _| {
            ready_during_write.store(
                SemanticPersistence::readiness(&local)?.ready,
                Ordering::Relaxed,
            );
            graph.create_node_with_props(&["DurablePublication"], [])?;
            Ok(())
        })
        .unwrap();
    assert!(!ready_during_write.load(Ordering::Relaxed));
    assert!(SemanticPersistence::readiness(&local).unwrap().ready);
}

#[test]
fn sql_publication_failure_cannot_be_cleared_by_an_unrelated_write() {
    let local = LocalPersistence::in_memory().unwrap();
    fail_sql_publication(&local);
    assert!(!SemanticPersistence::readiness(&local).unwrap().ready);
    let error = local
        .core
        .storage_manager()
        .semantic_storage()
        .commit_graph_write(|_, _, _| Ok(()))
        .unwrap_err();
    assert!(error.to_string().contains("publication") || error.to_string().contains("unresolved"));
    assert!(!SemanticPersistence::readiness(&local).unwrap().ready);
}

#[test]
fn reopening_does_not_guess_the_outcome_of_a_failed_publication() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("publication.db");
    let local = LocalPersistence::open(&path).unwrap();
    fail_sql_publication(&local);
    drop(local);
    let error = LocalPersistence::open(&path)
        .err()
        .expect("an unresolved publication must prevent reopening as a healthy database");
    assert!(error.to_string().contains("unresolved"), "{error}");
}

fn fail_sql_publication(local: &LocalPersistence) {
    let storage = local.core.storage_manager().semantic_storage();
    let initial = storage.graph.plan(|_| Ok(())).unwrap();
    let error = storage
        .commit_prepared_graph_write_with_sql(
            initial,
            |_| Ok(()),
            |graph, _, _, ()| {
                graph.create_node_with_props(&["UnpublishedGraph"], [])?;
                Ok(())
            },
            |_, _| Err(DbError::Grafeo("forced SQL publication failure".into())),
        )
        .unwrap_err();
    assert!(error.to_string().contains("forced SQL publication failure"));
}

/// One publication of a single node, the unit every WAL failure case injects into.
fn publish_probe(local: &LocalPersistence) -> crate::Result<()> {
    local
        .core
        .storage_manager()
        .semantic_storage()
        .commit_graph_write(|graph, _, _| {
            graph.create_node_with_props(&["NeverAccepted"], [])?;
            Ok(())
        })
}

// Grafeo 0.6 reaches two returned-error WAL points during a publication (`wal:write`, `wal:sync`);
// `wal:dir_sync` and `wal:truncate` need a segment rotation or a partial frame and are not reachable
// here. The loop walks every call number until the publication completes, so a point added or
// removed upstream changes the observed list and fails the assertion instead of passing vacuously.
#[test]
fn returned_wal_errors_never_acknowledge_a_durable_publication() {
    use grafeo_common::testing::crash::with_failure_at;
    let mut hit_points = Vec::new();
    for call in 1..=16 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("uncertain.db");
        let local = LocalPersistence::open(&path).unwrap();
        let Err(error) = with_failure_at(call, || publish_probe(&local)) else {
            assert!(SemanticPersistence::readiness(&local).unwrap().ready);
            break;
        };
        let message = error.to_string();
        let point = ["wal:write", "wal:sync"]
            .into_iter()
            .find(|point| message.contains(&format!("injected failure at: {point}")))
            .unwrap_or_else(|| panic!("call {call}: not an injected WAL failure: {message}"));
        hit_points.push(point);
        assert!(
            !SemanticPersistence::readiness(&local).unwrap().ready,
            "{point}: a failed WAL must not acknowledge the publication"
        );
        let unrelated = publish_probe(&local).unwrap_err().to_string();
        assert!(
            unrelated.contains("publication") || unrelated.contains("unresolved"),
            "{point}: {unrelated}"
        );
        drop(local);
        let reopen = LocalPersistence::open(&path)
            .err()
            .unwrap_or_else(|| panic!("{point}: uncertainty must survive reopen"));
        assert!(
            reopen.to_string().contains("unresolved"),
            "{point}: {reopen}"
        );
    }
    assert_eq!(hit_points, ["wal:write", "wal:sync"]);
}

#[test]
fn graph_commit_then_process_exit_leaves_a_durable_publication_fence() {
    let directory = tempfile::Builder::new()
        .prefix("lumvise-publication-crash-")
        .tempdir()
        .unwrap();
    let path = directory.path().join("publication.db");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "local::grafeo::committed_writes::durability_tests::exit_after_graph_commit",
            "--exact",
            "--ignored",
        ])
        .env("LUMVISE_PUBLICATION_CRASH_FIXTURE", &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let error = LocalPersistence::open(&path)
        .err()
        .expect("pending publication must survive process exit");
    assert!(error.to_string().contains("unresolved"), "{error}");
}

#[test]
#[ignore = "invoked only by its parent with an isolated new database"]
fn exit_after_graph_commit() {
    let path =
        std::path::PathBuf::from(std::env::var_os("LUMVISE_PUBLICATION_CRASH_FIXTURE").unwrap());
    assert!(path.is_absolute() && !path.exists());
    assert!(
        path.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("lumvise-publication-crash-")
    );
    let local = LocalPersistence::open(&path).unwrap();
    let storage = local.core.storage_manager().semantic_storage();
    let initial = storage.graph.plan(|_| Ok(())).unwrap();
    let result = storage.commit_prepared_graph_write_with_sql(
        initial,
        |_| Ok(()),
        |graph, _, _, ()| {
            graph.create_node_with_props(&["DurableButUnpublished"], [])?;
            Ok(())
        },
        |_, _| std::process::exit(0),
    );
    panic!("expected exit after graph commit, got {result:?}");
}

// Each crash point Grafeo 0.6 exposes during a publication is hit in a child process that exits
// inside the panic hook, so no `Drop` runs a clean close. After every crash a reopen must either
// refuse as unresolved or hold the whole publication exactly once: never a partial one.
#[test]
fn crashes_during_a_publication_never_leave_a_partial_publication() {
    let mut crash_points = Vec::new();
    for crash_at in 1..=32 {
        let directory = tempfile::Builder::new()
            .prefix("lumvise-wal-crash-")
            .tempdir()
            .unwrap();
        let path = directory.path().join("publication.db");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "local::grafeo::committed_writes::durability_tests::crash_during_publication",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("LUMVISE_WAL_CRASH_FIXTURE", &path)
            .env("LUMVISE_WAL_CRASH_AT", crash_at.to_string())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() {
            assert!(stderr.contains("publication completed"), "{stderr}");
            break;
        }
        assert_eq!(output.status.code(), Some(42), "{stderr}");
        let point = stderr
            .lines()
            .find_map(|line| {
                line.split_once("crash injection at: ")
                    .map(|(_, point)| point.trim())
            })
            .unwrap_or_else(|| panic!("crash {crash_at} did not name its point: {stderr}"))
            .to_owned();
        match LocalPersistence::open(&path) {
            Err(error) => assert!(error.to_string().contains("unresolved"), "{point}: {error}"),
            Ok(local) => {
                let nodes =
                    local
                        .core
                        .storage_manager()
                        .semantic_storage()
                        .graph
                        .read(|database| {
                            let snapshot = database.committed_snapshot().unwrap();
                            snapshot
                                .nodes_by_label("NeverAccepted", snapshot.view())
                                .unwrap()
                                .len()
                        });
                assert_eq!(
                    nodes, 1,
                    "{point}: healthy reopen must hold the whole publication"
                );
                assert!(
                    SemanticPersistence::readiness(&local).unwrap().ready,
                    "{point}"
                );
            }
        }
        crash_points.push(point);
    }
    for required in ["wal:after_frame", "wal:after_sync"] {
        assert!(
            crash_points.iter().any(|point| point == required),
            "{required} never fired: {crash_points:?}"
        );
    }
}

#[test]
#[ignore = "invoked only by its parent with an isolated new database"]
fn crash_during_publication() {
    let path = std::path::PathBuf::from(std::env::var_os("LUMVISE_WAL_CRASH_FIXTURE").unwrap());
    assert!(path.is_absolute() && !path.exists());
    assert!(
        path.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("lumvise-wal-crash-")
    );
    let crash_at = std::env::var("LUMVISE_WAL_CRASH_AT")
        .unwrap()
        .parse()
        .unwrap();
    let local = LocalPersistence::open(&path).unwrap();
    std::panic::set_hook(Box::new(|info| {
        eprintln!("{info}");
        std::process::exit(42);
    }));
    grafeo_common::testing::crash::enable_crash_at(crash_at);
    publish_probe(&local).unwrap();
    eprintln!("publication completed");
}
