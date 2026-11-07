use lumvise_db_core::{
    ArtifactTextVector, DbError, LocalPersistence, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult, StoredSemanticElementTextVector,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

const ROOT: &str = "/text-vectors";
const ENGINE: &str = "fastembed-local::bge-cls-v1";
const MODEL: &str = "bge-small-en-v1.5";

/// Versioned content fingerprints; the two values differ so a changed section is detectable.
const FP_A: &str = "fp1:000000000000000a:000000000000000a";
const FP_B: &str = "fp1:000000000000000b:000000000000000b";
const FP_OLD: &str = "fp1:0000000000000001:0000000000000001";

fn execute(
    persistence: &LocalPersistence,
    operation: SemanticOperation,
) -> Result<SemanticResult, DbError> {
    SemanticPersistence::execute(persistence, operation, &InvocationControl::sixty_seconds())
}

fn execute_ok(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    execute(persistence, operation).unwrap()
}

fn section(id: &str, kind: &str, fingerprint: Option<&str>) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "page".into(),
        path: "page.html".into(),
        element_kind: kind.into(),
        name: "Heading".into(),
        parent_element_id: None,
        content_fingerprint: fingerprint.map(str::to_string),
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn sync(persistence: &LocalPersistence, elements: Vec<SemanticElement>) {
    execute_ok(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: ROOT.into(),
            elements,
            relationships: vec![],
        },
    );
}

fn text_vector(id: &str, fingerprint: &str, vector: Vec<f32>) -> StoredSemanticElementTextVector {
    text_chunk(id, fingerprint, 0, vector)
}

fn text_chunk(
    id: &str,
    fingerprint: &str,
    chunk: u32,
    vector: Vec<f32>,
) -> StoredSemanticElementTextVector {
    StoredSemanticElementTextVector {
        semantic_element_id: id.into(),
        project_root: ROOT.into(),
        content_fingerprint: fingerprint.into(),
        chunk,
        vector: ArtifactTextVector {
            engine_id: ENGINE.into(),
            model: Some(MODEL.into()),
            dimensions: vector.len(),
            vector,
            normalized: true,
            metadata: json!({}),
        },
    }
}

fn store(
    persistence: &LocalPersistence,
    vectors: Vec<StoredSemanticElementTextVector>,
) -> Result<SemanticResult, DbError> {
    execute(
        persistence,
        SemanticOperation::StoreElementTextVectors {
            project_root: ROOT.into(),
            vectors,
        },
    )
}

fn search_hits(persistence: &LocalPersistence, query: Vec<f32>, k: usize) -> Vec<(String, f32)> {
    let SemanticResult::ElementVectorSearch(hits) = execute_ok(
        persistence,
        SemanticOperation::SearchElementTextVectors {
            project_root: ROOT.into(),
            query,
            k,
            engine_id: ENGINE.into(),
            model: Some(MODEL.into()),
        },
    ) else {
        panic!("expected element vector search");
    };
    hits.into_iter().map(|hit| (hit.id, hit.score)).collect()
}

fn search_ids(persistence: &LocalPersistence) -> Vec<String> {
    search_hits(persistence, vec![1.0, 0.0], 10)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn text_vector_round_trips_through_search_for_its_html_section() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_A))],
    );
    let result = store(
        &persistence,
        vec![text_vector("sec-a", FP_A, vec![1.0, 0.0])],
    )
    .unwrap();
    assert!(matches!(
        result,
        SemanticResult::StoredElementTextVectors { count: 1 }
    ));
    assert_eq!(search_ids(&persistence), vec!["sec-a".to_string()]);
}

#[test]
fn text_vector_write_rejects_non_html_section_element() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("fn-run", "function", Some(FP_A))],
    );
    assert!(
        store(
            &persistence,
            vec![text_vector("fn-run", FP_A, vec![1.0, 0.0])]
        )
        .is_err()
    );
    assert!(search_ids(&persistence).is_empty());
}

#[test]
fn text_vector_write_rejects_stale_fingerprint() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_B))],
    );
    assert!(
        store(
            &persistence,
            vec![text_vector("sec-a", FP_OLD, vec![1.0, 0.0])]
        )
        .is_err()
    );
    assert!(search_ids(&persistence).is_empty());
}

#[test]
fn text_vector_disappears_when_its_element_is_removed() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_A))],
    );
    store(
        &persistence,
        vec![text_vector("sec-a", FP_A, vec![1.0, 0.0])],
    )
    .unwrap();
    sync(&persistence, vec![]);
    assert!(search_ids(&persistence).is_empty());
}

#[test]
fn text_vector_is_not_returned_after_the_section_fingerprint_changes() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_A))],
    );
    store(
        &persistence,
        vec![text_vector("sec-a", FP_A, vec![1.0, 0.0])],
    )
    .unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_B))],
    );
    assert!(search_ids(&persistence).is_empty());
    store(
        &persistence,
        vec![text_vector("sec-a", FP_B, vec![1.0, 0.0])],
    )
    .unwrap();
    assert_eq!(search_ids(&persistence), vec!["sec-a".to_string()]);
}

#[test]
fn portable_archive_carries_no_text_vectors_and_import_does_not_restore_them() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.pz");
    let source = LocalPersistence::in_memory().unwrap();
    sync(&source, vec![section("sec-a", "html_section", Some(FP_A))]);
    store(&source, vec![text_vector("sec-a", FP_A, vec![1.0, 0.0])]).unwrap();
    assert_eq!(search_ids(&source), vec!["sec-a".to_string()]);

    let SemanticResult::PzSnapshot(snapshot) = execute_ok(
        &source,
        SemanticOperation::CreatePzSnapshot {
            project_root: ROOT.into(),
            output_path: path.to_string_lossy().into_owned(),
        },
    ) else {
        panic!("expected PZ snapshot");
    };
    assert!(
        snapshot
            .row_counts
            .keys()
            .all(|table| !table.contains("element_text")),
        "text vector tables found in archive: {:?}",
        snapshot.row_counts
    );

    let restored = LocalPersistence::in_memory().unwrap();
    execute_ok(
        &restored,
        SemanticOperation::ImportPzSnapshot {
            project_root: ROOT.into(),
            input_path: path.to_string_lossy().into_owned(),
        },
    );
    assert!(search_ids(&restored).is_empty());
}

#[test]
fn storing_one_chunk_replaces_every_earlier_chunk_of_the_element() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_A))],
    );
    store(
        &persistence,
        vec![
            text_chunk("sec-a", FP_A, 0, vec![1.0, 0.0]),
            text_chunk("sec-a", FP_A, 1, vec![0.0, 1.0]),
            text_chunk("sec-a", FP_A, 2, vec![0.0, 1.0]),
        ],
    )
    .unwrap();
    store(
        &persistence,
        vec![text_chunk("sec-a", FP_A, 0, vec![0.6, 0.8])],
    )
    .unwrap();
    // Chunks 1 and 2 would score 1.0 against this query if they had survived the replace.
    let hits = search_hits(&persistence, vec![0.0, 1.0], 10);
    assert_eq!(hits.len(), 1);
    assert!((hits[0].1 - 0.8).abs() < 1e-6, "score was {}", hits[0].1);
}

#[test]
fn text_vector_write_rejects_a_duplicate_chunk_of_one_element() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![section("sec-a", "html_section", Some(FP_A))],
    );
    let result = store(
        &persistence,
        vec![
            text_chunk("sec-a", FP_A, 0, vec![1.0, 0.0]),
            text_chunk("sec-a", FP_A, 0, vec![0.0, 1.0]),
        ],
    );
    assert!(result.is_err());
    assert!(search_hits(&persistence, vec![1.0, 0.0], 10).is_empty());
}

#[test]
fn search_returns_one_hit_per_element_scored_by_its_best_chunk() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        vec![
            section("sec-a", "html_section", Some(FP_A)),
            section("sec-b", "html_section", Some(FP_B)),
        ],
    );
    store(
        &persistence,
        vec![
            text_chunk("sec-a", FP_A, 0, vec![1.0, 0.0]),
            text_chunk("sec-a", FP_A, 1, vec![0.0, 1.0]),
            text_chunk("sec-b", FP_B, 0, vec![0.6, 0.8]),
        ],
    )
    .unwrap();
    let hits = search_hits(&persistence, vec![0.0, 1.0], 10);
    let ids = hits.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["sec-a", "sec-b"]);
    assert!((hits[0].1 - 1.0).abs() < 1e-6);
    assert!((hits[1].1 - 0.8).abs() < 1e-6);
    // k counts elements: sec-a's two chunks must not fill both slots.
    assert_eq!(search_hits(&persistence, vec![0.0, 1.0], 2).len(), 2);
    assert_eq!(search_hits(&persistence, vec![0.0, 1.0], 1)[0].0, "sec-a");
}
