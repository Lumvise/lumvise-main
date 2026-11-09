use super::*;
use crate::{
    ArtifactTextVector, ElementTextVectorGenerationKey as Key,
    ElementTextVectorGenerationSpec as Spec, ElementTextVectorGenerationState as State,
    ElementTextVectorGenerationStatus as Status, SemanticElement, StoredSemanticElementTextVector,
};
use serde_json::json;

const ROOT: &str = "/generation-project";
const FP: &str = "fp1:000000000000000a:000000000000000a";

fn section(fingerprint: &str) -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: "section".into(),
        semantic_source_id: "page".into(),
        path: "page.html".into(),
        element_kind: "html_section".into(),
        name: "Section".into(),
        parent_element_id: None,
        revision_dependencies: Vec::new(),
        content_fingerprint: Some(fingerprint.into()),
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn spec() -> Spec {
    Spec {
        key: Key {
            project_root: ROOT.into(),
            semantic_element_id: "section".into(),
            generation_id: "attempt".into(),
        },
        content_fingerprint: FP.into(),
        engine_id: "section-test".into(),
        model: Some("test-model".into()),
        profile_fingerprint: "profile".into(),
        dimensions: 2,
        chunk_count: 2,
        chunk_plan_fingerprint: "exact-ranges".into(),
    }
}

fn execute(db: &LocalPersistence, operation: SemanticOperation) -> Result<SemanticResult> {
    SemanticPersistence::execute(db, operation, &InvocationControl::sixty_seconds())
}

fn sync(db: &LocalPersistence, elements: Vec<SemanticElement>) {
    execute(
        db,
        SemanticOperation::SyncStructure {
            project_root: ROOT.into(),
            elements,
            relationships: Vec::new(),
        },
    )
    .unwrap();
}

fn fixture() -> LocalPersistence {
    let db = LocalPersistence::in_memory().unwrap();
    sync(&db, vec![section(FP)]);
    db
}

fn status(result: Result<SemanticResult>) -> Status {
    match result.unwrap() {
        SemanticResult::ElementTextVectorGeneration(status) => status,
        other => panic!("{other:?}; expected generation status"),
    }
}

fn begin(db: &LocalPersistence, spec: &Spec) -> Result<SemanticResult> {
    execute(
        db,
        SemanticOperation::BeginElementTextVectorGeneration { spec: spec.clone() },
    )
}

fn stage(db: &LocalPersistence, ordinal: u32, vector: &[f32]) -> Result<SemanticResult> {
    execute(
        db,
        SemanticOperation::StageElementTextVectorChunk {
            key: spec().key,
            chunk: ordinal,
            vector: vector.to_vec(),
        },
    )
}

fn commit(db: &LocalPersistence) -> Status {
    status(execute(
        db,
        SemanticOperation::CommitElementTextVectorGeneration { key: spec().key },
    ))
}

fn abort(db: &LocalPersistence) -> Status {
    status(execute(
        db,
        SemanticOperation::AbortElementTextVectorGeneration { key: spec().key },
    ))
}

fn complete_staging(db: &LocalPersistence) {
    status(stage(db, 0, &[0.0, 1.0]));
    status(stage(db, 1, &[0.0, 1.0]));
}

fn store_complete(db: &LocalPersistence, vector: &[f32]) {
    let spec = spec();
    let record = StoredSemanticElementTextVector {
        project_root: ROOT.into(),
        semantic_element_id: "section".into(),
        content_fingerprint: FP.into(),
        chunk: 0,
        vector: ArtifactTextVector {
            engine_id: spec.engine_id,
            model: spec.model,
            dimensions: vector.len(),
            vector: vector.to_vec(),
            normalized: false,
            metadata: json!({}),
        },
    };
    execute(
        db,
        SemanticOperation::StoreElementTextVectors {
            project_root: ROOT.into(),
            vectors: vec![record],
        },
    )
    .unwrap();
}

fn scores(db: &LocalPersistence) -> Vec<f32> {
    match execute(
        db,
        SemanticOperation::SearchElementTextVectors {
            project_root: ROOT.into(),
            query: vec![1.0, 0.0],
            k: 10,
            engine_id: spec().engine_id,
            model: spec().model,
        },
    )
    .unwrap()
    {
        SemanticResult::ElementVectorSearch(hits) => {
            hits.into_iter().map(|hit| hit.score).collect()
        }
        other => panic!("{other:?}; expected vector search"),
    }
}

#[test]
fn generation_publishes_complete_set_only_and_keeps_lost_ack_receipt() {
    let db = fixture();
    store_complete(&db, &[1.0, 0.0]);
    assert_eq!(status(begin(&db, &spec())).state, State::Open);
    assert_eq!(status(stage(&db, 0, &[0.0, 1.0])).next_chunk, 1);
    assert_eq!(scores(&db), vec![1.0]);
    assert!(
        execute(
            &db,
            SemanticOperation::CommitElementTextVectorGeneration { key: spec().key }
        )
        .is_err()
    );
    status(stage(&db, 1, &[0.0, 1.0]));
    assert_eq!(scores(&db), vec![1.0]);
    let published = commit(&db);
    assert_eq!(
        (published.state, published.published_count),
        (State::Committed, 2)
    );
    assert_eq!(scores(&db), vec![0.0]);
    assert_eq!(commit(&db), published);
    assert_eq!(status(begin(&db, &spec())), published);
    assert_eq!(status(stage(&db, 0, &[0.0, 1.0])), published);
    assert_eq!(abort(&db), published);
    assert!(stage(&db, 0, &[1.0, 0.0]).is_err());
}

#[test]
fn generation_validates_scope_immutable_inputs_consecutive_chunks_and_values() {
    let db = fixture();
    let mut wrong = spec();
    wrong.key.project_root = "/foreign-project".into();
    assert!(begin(&db, &wrong).is_err());
    wrong = spec();
    wrong.content_fingerprint = "different".into();
    assert!(begin(&db, &wrong).is_err());
    wrong = spec();
    wrong.dimensions = 0;
    assert!(begin(&db, &wrong).is_err());
    status(begin(&db, &spec()));
    wrong = spec();
    wrong.key.generation_id = "second".into();
    assert!(begin(&db, &wrong).is_err());
    wrong = spec();
    wrong.profile_fingerprint = "changed".into();
    assert!(begin(&db, &wrong).is_err());
    for (chunk, vector) in [
        (1, vec![1.0, 0.0]),
        (2, vec![1.0, 0.0]),
        (0, vec![1.0]),
        (0, vec![f32::NAN, 0.0]),
    ] {
        let error = stage(&db, chunk, &vector).unwrap_err().to_string();
        assert!(error.contains("expected"), "{error}");
        assert!(error.contains(&chunk.to_string()), "{error}");
    }
    let first = status(stage(&db, 0, &[1.0, 0.0]));
    assert_eq!(status(stage(&db, 0, &[1.0, 0.0])), first);
    assert!(stage(&db, 0, &[0.0, 1.0]).is_err());
}

#[test]
fn full_source_fingerprint_and_concurrent_complete_replacements_are_cas_fences() {
    for replacement in [false, true] {
        let db = fixture();
        store_complete(&db, &[1.0, 0.0]);
        status(begin(&db, &spec()));
        complete_staging(&db);
        if replacement {
            store_complete(&db, &[1.0, 0.0]);
        } else {
            sync(&db, vec![section("fp1:000000000000000a:000000000000000b")]);
        }
        assert_eq!(commit(&db).state, State::Conflicted);
        assert_eq!(commit(&db).state, State::Conflicted);
        if replacement {
            assert_eq!(scores(&db), vec![1.0]);
        } else {
            assert!(scores(&db).is_empty());
        }
    }
}

#[test]
fn abort_is_terminal_without_erasing_published_vectors() {
    let db = fixture();
    store_complete(&db, &[1.0, 0.0]);
    status(begin(&db, &spec()));
    status(stage(&db, 0, &[0.0, 1.0]));
    let stopped = abort(&db);
    assert_eq!(stopped.state, State::Aborted);
    assert_eq!(abort(&db), stopped);
    assert_eq!(status(begin(&db, &spec())), stopped);
    assert_eq!(commit(&db), stopped);
    assert_eq!(status(stage(&db, 1, &[0.0, 1.0])), stopped);
    assert_eq!(scores(&db), vec![1.0]);
}

#[test]
fn restart_resumes_hidden_chunks_and_terminal_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("generation.sqlite");
    {
        let db = LocalPersistence::open(&path).unwrap();
        sync(&db, vec![section(FP)]);
        status(begin(&db, &spec()));
        status(stage(&db, 0, &[0.0, 1.0]));
    }
    {
        let db = LocalPersistence::open(&path).unwrap();
        assert_eq!(status(begin(&db, &spec())).next_chunk, 1);
        assert!(scores(&db).is_empty());
        status(stage(&db, 1, &[0.0, 1.0]));
        assert_eq!(commit(&db).state, State::Committed);
    }
    let db = LocalPersistence::open(&path).unwrap();
    assert_eq!(commit(&db).published_count, 2);
    assert_eq!(scores(&db), vec![0.0]);
}

struct FakeGenerationClock(std::sync::atomic::AtomicI64);
impl crate::local::clock::Clock for FakeGenerationClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(self.0.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap()
    }
}

fn with_clock(path: &std::path::Path, clock: Arc<FakeGenerationClock>) -> LocalPersistence {
    LocalPersistence {
        core: Arc::new(DbCore::open_with_clock(path, clock).unwrap()),
        staged_snapshots: Mutex::new(HashMap::new()),
    }
}

#[test]
fn idle_expiry_on_open_preserves_live_vectors_and_terminal_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("expiry.sqlite");
    let clock = Arc::new(FakeGenerationClock(std::sync::atomic::AtomicI64::new(
        1_000,
    )));
    {
        let db = with_clock(&path, clock.clone());
        sync(&db, vec![section(FP)]);
        store_complete(&db, &[1.0, 0.0]);
        status(begin(&db, &spec()));
        status(stage(&db, 0, &[0.0, 1.0]));
    }
    clock
        .0
        .store(86_401_000, std::sync::atomic::Ordering::SeqCst);
    let db = with_clock(&path, clock);
    assert_eq!(status(begin(&db, &spec())).state, State::Expired);
    assert_eq!(commit(&db).state, State::Expired);
    assert_eq!(status(stage(&db, 1, &[0.0, 1.0])).state, State::Expired);
    assert!(stage(&db, 0, &[1.0, 0.0]).is_err());
    assert_eq!(hidden_chunks(&db), 0);
    assert_eq!(scores(&db), vec![1.0]);
}

fn hidden_chunks(db: &LocalPersistence) -> usize {
    db.core
        .graph
        .read(|graph| -> Result<usize> {
            let snapshot = graph
                .snapshot()
                .map_err(|error| DbError::Grafeo(error.to_string()))?;
            let ids = snapshot
                .nodes_by_label("ElementTextVectorStage", snapshot.view())
                .map_err(|error| DbError::Grafeo(error.to_string()))?;
            Ok(ids.len())
        })
        .unwrap()
}

#[test]
fn deletion_and_project_replacement_remove_hidden_state_and_receipts() {
    for explicit in [false, true] {
        let db = fixture();
        status(begin(&db, &spec()));
        complete_staging(&db);
        if explicit {
            execute(
                &db,
                SemanticOperation::RemoveElement {
                    semantic_element_id: "section".into(),
                },
            )
            .unwrap();
        } else {
            sync(&db, Vec::new());
        }
        assert_eq!(hidden_chunks(&db), 0);
        assert!(
            execute(
                &db,
                SemanticOperation::CommitElementTextVectorGeneration { key: spec().key }
            )
            .is_err()
        );
        assert!(begin(&db, &spec()).is_err());
        sync(&db, vec![section(FP)]);
        assert_eq!(status(begin(&db, &spec())).next_chunk, 0);
    }
}

#[test]
fn staged_vectors_do_not_enter_pz_or_source_change_elements() {
    let db = fixture();
    let SemanticResult::SemanticRevision { commit_version } =
        execute(&db, SemanticOperation::SemanticRevision).unwrap()
    else {
        panic!("expected revision");
    };
    status(begin(&db, &spec()));
    complete_staging(&db);
    let changes = execute(
        &db,
        SemanticOperation::ChangesSinceRevision {
            scope: crate::ChangeHookScope {
                project_root: ROOT.into(),
                ..Default::default()
            },
            after_revision: commit_version,
            limit: 100,
        },
    )
    .unwrap();
    let SemanticResult::ChangesSinceRevision(changes) = changes else {
        panic!("expected source change page");
    };
    assert!(changes.changed.is_empty());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("staged.pz");
    execute(
        &db,
        SemanticOperation::CreatePzSnapshot {
            project_root: ROOT.into(),
            output_path: path.to_string_lossy().into_owned(),
        },
    )
    .unwrap();
    let restored = LocalPersistence::in_memory().unwrap();
    execute(
        &restored,
        SemanticOperation::ImportPzSnapshot {
            project_root: ROOT.into(),
            input_path: path.to_string_lossy().into_owned(),
        },
    )
    .unwrap();
    assert!(scores(&restored).is_empty());
    assert_eq!(hidden_chunks(&restored), 0);
    assert!(
        execute(
            &restored,
            SemanticOperation::CommitElementTextVectorGeneration { key: spec().key }
        )
        .is_err()
    );
    assert_eq!(status(begin(&restored, &spec())).next_chunk, 0);
}

#[test]
fn central_resource_encoding_refuses_local_only_generation_operations() {
    for operation in [
        SemanticOperation::BeginElementTextVectorGeneration { spec: spec() },
        SemanticOperation::StageElementTextVectorChunk {
            key: spec().key,
            chunk: 0,
            vector: vec![1.0, 0.0],
        },
        SemanticOperation::CommitElementTextVectorGeneration { key: spec().key },
        SemanticOperation::AbortElementTextVectorGeneration { key: spec().key },
    ] {
        let error = crate::CentralizedPersistence::encode_semantic_operation(operation)
            .unwrap_err()
            .to_string();
        assert!(error.contains("local persistence"), "{error}");
    }
}

#[test]
fn generation_operations_preserve_explicit_project_routes() {
    for operation in [
        SemanticOperation::BeginElementTextVectorGeneration { spec: spec() },
        SemanticOperation::StageElementTextVectorChunk {
            key: spec().key,
            chunk: 0,
            vector: vec![1.0, 0.0],
        },
        SemanticOperation::CommitElementTextVectorGeneration { key: spec().key },
        SemanticOperation::AbortElementTextVectorGeneration { key: spec().key },
    ] {
        assert_eq!(operation.project_root(), Some(ROOT));
    }
}

#[test]
fn old_unchunked_live_node_reopens_and_complete_store_still_replaces_all() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("old.sqlite");
    {
        let db = LocalPersistence::open(&path).unwrap();
        sync(&db, vec![section(FP)]);
        db.core
            .storage_manager()
            .semantic_storage()
            .commit_graph_write(|graph, _, _| {
                graph.create_node_with_props(
                    &["SemanticElementTextVector"],
                    [
                        ("semantic_element_id", grafeo::Value::from("section")),
                        ("project_root", grafeo::Value::from(ROOT)),
                        ("content_fingerprint", grafeo::Value::from(FP)),
                        ("engine_id", grafeo::Value::from(spec().engine_id)),
                        ("model", grafeo::Value::from(spec().model.unwrap())),
                        ("dimensions", grafeo::Value::from(2_i64)),
                        ("vector", grafeo::Value::Vector(vec![1.0_f32, 0.0].into())),
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }
    let db = LocalPersistence::open(&path).unwrap();
    assert_eq!(scores(&db), vec![1.0]);
    status(begin(&db, &spec()));
    complete_staging(&db);
    store_complete(&db, &[1.0, 0.0]);
    assert_eq!(commit(&db).state, State::Conflicted);
    assert_eq!(scores(&db), vec![1.0]);
}

#[test]
fn abort_before_first_chunk_and_delayed_stage_are_terminal_without_publication() {
    let db = fixture();
    status(begin(&db, &spec()));
    let stopped = abort(&db);
    let before = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    assert_eq!(status(stage(&db, 0, &[1.0, 0.0])), stopped);
    let after = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(hidden_chunks(&db), 0);
    assert_eq!(status(begin(&db, &spec())), stopped);
}

#[test]
fn staged_chunks_are_native_vectors_without_json_payloads() {
    let db = fixture();
    status(begin(&db, &spec()));
    status(stage(&db, 0, &[1.0, 0.0]));
    db.core.graph.read(|graph| -> Result<()> {
        let snapshot=graph.snapshot().map_err(|error| DbError::Grafeo(error.to_string()))?;
        let ids=snapshot.nodes_by_label("ElementTextVectorStage",snapshot.view()).map_err(|error| DbError::Grafeo(error.to_string()))?;
        assert_eq!(ids.len(),1);
        let node=snapshot.node(ids[0],snapshot.view()).map_err(|error| DbError::Grafeo(error.to_string()))?.unwrap();
        assert!(matches!(node.get_property("vector"),Some(grafeo::Value::Vector(values)) if values.as_ref()==[1.0,0.0]));
        assert!(matches!(node.get_property("chunk"),Some(grafeo::Value::Int64(0))));
        assert!(node.get_property("chunk_json").is_none());
        Ok(())
    }).unwrap();
}

#[test]
fn cancelled_or_expired_generation_access_does_not_run_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cancel.sqlite");
    let clock = Arc::new(FakeGenerationClock(std::sync::atomic::AtomicI64::new(
        1_000,
    )));
    let db = with_clock(&path, clock.clone());
    sync(&db, vec![section(FP)]);
    status(begin(&db, &spec()));
    status(stage(&db, 0, &[1.0, 0.0]));
    clock
        .0
        .store(86_401_000, std::sync::atomic::Ordering::SeqCst);
    let cancelled = InvocationControl::sixty_seconds();
    cancelled.cancel();
    let expired = InvocationControl::with_deadline(Duration::ZERO);
    for control in [cancelled, expired] {
        let error = SemanticPersistence::execute(
            &db,
            SemanticOperation::CommitElementTextVectorGeneration { key: spec().key },
            &control,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("invocation"), "{error}");
        assert_eq!(hidden_chunks(&db), 1);
    }
    assert_eq!(status(begin(&db, &spec())).state, State::Expired);
    assert_eq!(hidden_chunks(&db), 0);
}

#[test]
fn abort_before_begin_is_a_durable_key_only_terminal_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("prebegin.sqlite");
    let expected = Status {
        key: spec().key,
        state: State::Aborted,
        next_chunk: 0,
        chunk_count: 0,
        published_count: 0,
    };
    {
        let db = LocalPersistence::open(&path).unwrap();
        sync(&db, vec![section(FP)]);
        assert_eq!(abort(&db), expected);
        assert_eq!(hidden_chunks(&db), 0);
        db.core
            .graph
            .read(|graph| -> Result<()> {
                let snapshot = graph
                    .snapshot()
                    .map_err(|error| DbError::Grafeo(error.to_string()))?;
                let nodes = snapshot
                    .nodes_by_label("ElementTextVectorGeneration", snapshot.view())
                    .map_err(|error| DbError::Grafeo(error.to_string()))?;
                assert_eq!(nodes.len(), 1);
                let node = snapshot
                    .node(nodes[0], snapshot.view())
                    .map_err(|error| DbError::Grafeo(error.to_string()))?
                    .unwrap();
                let Some(grafeo::Value::String(body)) = node.get_property("receipt_json") else {
                    panic!("expected receipt JSON");
                };
                let receipt: serde_json::Value = serde_json::from_str(body).unwrap();
                assert_eq!(receipt["key"]["generation_id"], "attempt");
                assert!(!receipt.as_object().unwrap().contains_key("spec"));
                assert!(
                    !receipt
                        .as_object()
                        .unwrap()
                        .contains_key("published_digest")
                );
                Ok(())
            })
            .unwrap();
    }
    let db = LocalPersistence::open(&path).unwrap();
    let before = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    assert_eq!(abort(&db), expected);
    assert_eq!(status(begin(&db, &spec())), expected);
    assert_eq!(status(stage(&db, 0, &[1.0, 0.0])), expected);
    assert_eq!(commit(&db), expected);
    let after = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(hidden_chunks(&db), 0);
    assert!(scores(&db).is_empty());
    let mut fresh = spec();
    fresh.key.generation_id = "fresh-attempt".into();
    assert_eq!(status(begin(&db, &fresh)).state, State::Open);
}

#[test]
fn abort_before_begin_rejects_invalid_key_without_publication() {
    let db = fixture();
    let before = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    for field in ["project_root", "semantic_element_id", "generation_id"] {
        let mut key = spec().key;
        match field {
            "project_root" => key.project_root.clear(),
            "semantic_element_id" => key.semantic_element_id.clear(),
            _ => key.generation_id.clear(),
        }
        let error = execute(
            &db,
            SemanticOperation::AbortElementTextVectorGeneration { key },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("non-empty generation identity field"),
            "{error}"
        );
    }
    let after = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[test]
fn concurrent_abort_and_begin_cannot_reopen_a_cancelled_generation() {
    let db = Arc::new(fixture());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let begin_barrier = barrier.clone();
        let begin_db = db.clone();
        let starter = scope.spawn(move || {
            begin_barrier.wait();
            status(begin(&begin_db, &spec()))
        });
        let abort_db = db.clone();
        let canceller = scope.spawn(move || {
            barrier.wait();
            abort(&abort_db)
        });
        let began = starter.join().unwrap();
        let stopped = canceller.join().unwrap();
        assert!(matches!(began.state, State::Open | State::Aborted));
        assert_eq!(stopped.state, State::Aborted);
    });
    assert_eq!(status(begin(&db, &spec())).state, State::Aborted);
    assert_eq!(commit(&db).state, State::Aborted);
    assert_eq!(hidden_chunks(&db), 0);
}

#[test]
fn deleting_the_element_removes_a_prebegin_abort_tombstone() {
    let db = fixture();
    assert_eq!(abort(&db).chunk_count, 0);
    execute(
        &db,
        SemanticOperation::RemoveElement {
            semantic_element_id: "section".into(),
        },
    )
    .unwrap();
    sync(&db, vec![section(FP)]);
    assert_eq!(status(begin(&db, &spec())).state, State::Open);
}

#[test]
fn absent_element_deletion_cleans_abort_tombstone_without_source_event_or_repeat_write() {
    let db = LocalPersistence::in_memory().unwrap();
    assert_eq!(abort(&db).state, State::Aborted);
    let SemanticResult::SemanticRevision { commit_version } =
        execute(&db, SemanticOperation::SemanticRevision).unwrap()
    else {
        panic!("expected revision");
    };
    let removal = execute(
        &db,
        SemanticOperation::RemoveElement {
            semantic_element_id: "section".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        removal,
        SemanticResult::Removed { removed: false }
    ));
    let changes = execute(
        &db,
        SemanticOperation::ChangesSinceRevision {
            scope: crate::ChangeHookScope {
                project_root: ROOT.into(),
                ..Default::default()
            },
            after_revision: commit_version,
            limit: 100,
        },
    )
    .unwrap();
    let SemanticResult::ChangesSinceRevision(changes) = changes else {
        panic!("expected source changes");
    };
    assert!(changes.changed.is_empty());
    assert!(
        execute(
            &db,
            SemanticOperation::CommitElementTextVectorGeneration { key: spec().key }
        )
        .is_err()
    );
    let before = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    let again = execute(
        &db,
        SemanticOperation::RemoveElement {
            semantic_element_id: "section".into(),
        },
    )
    .unwrap();
    assert!(matches!(again, SemanticResult::Removed { removed: false }));
    let after = execute(&db, SemanticOperation::SemanticRevision).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    sync(&db, vec![section(FP)]);
    assert_eq!(status(begin(&db, &spec())).state, State::Open);
}
