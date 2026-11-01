use super::{InvocationControl, LocalPersistence, SemanticOperation, SemanticPersistence};
use crate::{DbError, Result, SemanticElement, SemanticResult};
use serde_json::json;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

struct HeldSearchPublication {
    release: Option<mpsc::Sender<()>>,
    writer: Option<JoinHandle<Result<()>>>,
}

impl HeldSearchPublication {
    fn start(persistence: Arc<LocalPersistence>, rollback: bool) -> Self {
        let (entered, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            persistence
                .core
                .storage_manager()
                .semantic_storage()
                .commit_graph_write(|graph, _, _| {
                    let node = graph.find_nodes_by_property(
                        "semantic_element_id",
                        &grafeo::Value::from("search-owner"),
                    )[0];
                    graph.set_node_property(node, "name", grafeo::Value::from("shared_during"))?;
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    if rollback {
                        return Err(DbError::invalid_value(
                            "injected failure",
                            "successful write",
                        ));
                    }
                    Ok(())
                })
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            release: Some(release),
            writer: Some(writer),
        }
    }

    fn finish(mut self) -> Result<()> {
        self.release.take().unwrap().send(()).unwrap();
        self.writer.take().unwrap().join().unwrap()
    }
}

impl Drop for HeldSearchPublication {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn search_fixture() -> (tempfile::TempDir, Arc<LocalPersistence>) {
    let directory = tempfile::tempdir().unwrap();
    let persistence = Arc::new(LocalPersistence::open(directory.path().join("search.db")).unwrap());
    for root in ["/search", "/other"] {
        persistence
            .execute(
                SemanticOperation::SyncStructure {
                    project_root: root.into(),
                    elements: vec![search_element(root)],
                    relationships: vec![],
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
    }
    (directory, persistence)
}

fn search_element(root: &str) -> SemanticElement {
    SemanticElement {
        semantic_element_id: if root == "/search" {
            "search-owner"
        } else {
            "other-owner"
        }
        .into(),
        project_root: root.into(),
        semantic_source_id: "live-search".into(),
        path: "src/search.rs".into(),
        element_kind: "function".into(),
        name: "shared_before".into(),
        parent_element_id: None,
        revision_dependencies: vec![],
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn run_held_search(operation: SemanticOperation, rollback: bool) -> SemanticResult {
    let (_directory, persistence) = search_fixture();
    run_held_search_with(persistence, operation, rollback)
}

fn run_held_search_with(
    persistence: Arc<LocalPersistence>,
    operation: SemanticOperation,
    rollback: bool,
) -> SemanticResult {
    let publication = HeldSearchPublication::start(persistence.clone(), rollback);
    let (finished, result) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        finished
            .send(persistence.execute(operation, &InvocationControl::sixty_seconds()))
            .unwrap();
    });
    let observed = result.recv_timeout(Duration::from_secs(2));
    let published = publication.finish();
    reader.join().unwrap();
    assert_eq!(published.is_err(), rollback);
    observed
        .expect("live search must finish before the publication gate is released")
        .unwrap()
}

#[test]
fn graph_search_does_not_wait_for_the_in_memory_sql_writer() {
    let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
    persistence
        .execute(
            SemanticOperation::SyncStructure {
                project_root: "/search".into(),
                elements: vec![search_element("/search")],
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let operation = SemanticOperation::SearchProjectStructure {
        project_root: "/search".into(),
    };
    let during = run_held_search_with(persistence.clone(), operation.clone(), false);
    let SemanticResult::ProjectSnapshot(during) = during else {
        panic!("expected live structure")
    };
    let SemanticResult::ProjectSnapshot(after) = persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
    else {
        panic!("expected live structure")
    };
    assert_eq!(during.commit_version, 1);
    assert_eq!(after.commit_version, 2);
    let strong = persistence
        .execute(
            SemanticOperation::ProjectSnapshot {
                scope: crate::ProjectSnapshotScope::ProjectRoot("/search".into()),
                artifact_namespace: None,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let SemanticResult::ProjectSnapshot(strong) = strong else {
        panic!("expected published snapshot")
    };
    assert_eq!(after.published_at, strong.published_at);
}

#[test]
fn existing_project_membership_remains_readable_during_publication() {
    let (_directory, persistence) = search_fixture();
    let operation = SemanticOperation::ProjectAccess {
        project_root: "/search".into(),
    };
    let expected = persistence
        .execute(operation.clone(), &InvocationControl::sixty_seconds())
        .unwrap();
    let observed = run_held_search_with(persistence, operation, false);
    let (SemanticResult::ProjectAccess(expected), SemanticResult::ProjectAccess(observed)) =
        (expected, observed)
    else {
        panic!("expected project membership");
    };
    assert_eq!(expected, observed);
}

#[test]
fn lexical_search_finishes_during_publication_and_stays_in_its_project() {
    for rollback in [false, true] {
        let result = run_held_search(
            SemanticOperation::SearchElementCandidates {
                project_root: Some("/search".into()),
                query: "shared".into(),
                limit: 10,
            },
            rollback,
        );
        let SemanticResult::Elements(elements) = result else {
            panic!("expected search elements")
        };
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].project_root, "/search");
    }
}

#[test]
fn graph_search_finishes_during_publication_without_exporting_artifacts() {
    for rollback in [false, true] {
        let result = run_held_search(
            SemanticOperation::SearchProjectStructure {
                project_root: "/search".into(),
            },
            rollback,
        );
        let SemanticResult::ProjectSnapshot(snapshot) = result else {
            panic!("expected search structure")
        };
        assert_eq!(snapshot.project_root, "/search");
        assert_eq!(snapshot.elements.len(), 1);
        assert_eq!(snapshot.elements[0].project_root, "/search");
    }
}

#[test]
fn live_search_cache_is_rebuilt_after_commit_and_rollback() {
    for rollback in [false, true] {
        let (_directory, persistence) = search_fixture();
        let publication = HeldSearchPublication::start(persistence.clone(), rollback);
        let operation = SemanticOperation::SearchElementCandidates {
            project_root: Some("/search".into()),
            query: "shared".into(),
            limit: 10,
        };
        let (finished, result) = mpsc::channel();
        let reader_db = persistence.clone();
        let reading = operation.clone();
        let reader = std::thread::spawn(move || {
            finished
                .send(reader_db.execute(reading, &InvocationControl::sixty_seconds()))
                .unwrap();
        });
        let observed = result.recv_timeout(Duration::from_secs(2));
        assert_eq!(publication.finish().is_err(), rollback);
        reader.join().unwrap();
        observed
            .expect("search must finish before the writer releases")
            .unwrap();
        let SemanticResult::Elements(elements) = persistence
            .execute(operation, &InvocationControl::sixty_seconds())
            .unwrap()
        else {
            panic!("expected search elements")
        };
        assert_eq!(
            elements[0].name,
            if rollback {
                "shared_before"
            } else {
                "shared_during"
            }
        );
    }
}

#[test]
fn vector_searches_finish_during_publication() {
    for operation in [
        SemanticOperation::SearchElementNameVectors {
            project_root: "/search".into(),
            query: vec![1.0, 0.0],
            k: 10,
            engine_id: "test".into(),
            model: None,
        },
        SemanticOperation::SearchArtifactTextVectors {
            project_root: "/search".into(),
            query: vec![1.0, 0.0],
            k: 10,
            engine_id: "test".into(),
            model: None,
        },
    ] {
        assert!(matches!(
            run_held_search(operation, false),
            SemanticResult::ElementVectorSearch(_) | SemanticResult::ArtifactVectorSearch(_)
        ));
    }
}

#[test]
fn confirmed_rollback_does_not_block_the_next_semantic_write() {
    let (_directory, persistence) = search_fixture();
    let publication = HeldSearchPublication::start(persistence.clone(), true);
    assert!(publication.finish().is_err());
    let mut next = search_element("/search");
    next.name = "after_recovery".into();
    persistence
        .execute(
            SemanticOperation::SyncStructure {
                project_root: "/search".into(),
                elements: vec![next],
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .expect("a verified rollback must not require an app restart before the next write");
}
