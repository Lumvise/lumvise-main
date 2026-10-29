use lumvise_db_core::{
    LocalPersistence, SemanticArtifact, SemanticElement, SemanticOperation, SemanticPersistence,
    SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::time::Instant;

fn element(root: &str, id: &str, name: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: root.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "search-test".into(),
        path: format!("src/{id}.rs"),
        element_kind: "function".into(),
        name: name.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"description": "x".repeat(4096)}),
    }
}

fn synchronize(persistence: &LocalPersistence, root: &str, elements: Vec<SemanticElement>) {
    let result = persistence
        .execute(
            SemanticOperation::SyncStructure {
                project_root: root.into(),
                elements,
                relationships: vec![],
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert!(matches!(result, SemanticResult::SyncStructure(_)));
}

fn search(
    persistence: &LocalPersistence,
    root: Option<&str>,
    query: &str,
    limit: usize,
) -> Vec<SemanticElement> {
    let result = persistence
        .execute(
            SemanticOperation::SearchElementCandidates {
                project_root: root.map(str::to_owned),
                query: query.into(),
                limit,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    match result {
        SemanticResult::Elements(elements) => elements,
        unexpected => panic!("expected semantic elements, got {unexpected:?}"),
    }
}

#[test]
fn public_search_preserves_rank_scope_metadata_and_observes_renames_and_deletions() {
    let persistence = LocalPersistence::in_memory().unwrap();
    synchronize(
        &persistence,
        "/a",
        vec![
            element("/a", "exact", "run_task"),
            element("/a", "partial", "run"),
        ],
    );
    synchronize(&persistence, "/b", vec![element("/b", "other", "run_task")]);
    let found = search(&persistence, Some("/a"), "RUN_TASK", 1);
    assert_eq!(found, [element("/a", "exact", "run_task")]);
    assert_eq!(search(&persistence, None, "run_task", 10).len(), 3);
    synchronize(&persistence, "/a", vec![element("/a", "exact", "changed")]);
    assert!(search(&persistence, Some("/a"), "run_task", 10).is_empty());
    assert_eq!(
        search(&persistence, Some("/a"), "changed", 1)[0].name,
        "changed"
    );
    assert!(search(&persistence, Some("/a"), "changed", 0).is_empty());
}

#[test]
#[ignore = "release retrieval evidence; run with --release --ignored --nocapture"]
fn semantic_search_cold_and_warm_samples() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let elements = (0..100_000)
        .map(|index| {
            element(
                "/benchmark",
                &format!("semantic:01234567-89ab-cdef-0123-{index:012}:function:/src/component{index}/module.rs:1"),
                &format!("function{index:05}"),
            )
        })
        .collect();
    synchronize(&persistence, "/benchmark", elements);
    for sample in 0..6 {
        let started = Instant::now();
        let found = search(&persistence, Some("/benchmark"), "function99999", 5);
        let elapsed_ns = started.elapsed().as_nanos();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "function99999");
        eprintln!(
            "{{\"workload\":\"semantic_search_100k\",\"sample\":{sample},\"elapsed_ns\":{elapsed_ns}}}"
        );
    }
}

#[test]
#[ignore = "release artifact retrieval evidence; run with --release --ignored --nocapture"]
fn artifact_search_owner_lookup_samples() {
    let persistence = LocalPersistence::in_memory().unwrap();
    synchronize(
        &persistence,
        "/benchmark",
        (0..100_000)
            .map(|index| element("/benchmark", &format!("id{index}"), "function"))
            .collect(),
    );
    for index in 0..100 {
        persistence
            .execute(
                SemanticOperation::UpsertArtifact {
                    artifact: SemanticArtifact {
                        artifact_id: format!("note{index}"),
                        semantic_element_id: format!("id{index}"),
                        artifact_kind: "knowledge".into(),
                        title: format!("Note {index}"),
                        content_ref: None,
                        content: Some("hello".into()),
                        searchable_text: None,
                        content_size_bytes: None,
                        dependencies: vec![],
                        metadata: json!({"knowledge": {}}),
                    },
                    media_type: "text/plain".into(),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
    }
    for sample in 0..3 {
        let started = Instant::now();
        let result = persistence
            .execute(
                SemanticOperation::ProjectArtifacts {
                    project_root: "/benchmark".into(),
                    artifact_namespace: Some("knowledge".into()),
                },
                &InvocationControl::sixty_seconds(),
            )
            .unwrap();
        let elapsed_ns = started.elapsed().as_nanos();
        assert!(matches!(result, SemanticResult::Artifacts(ref rows) if rows.len() == 100));
        eprintln!(
            "{{\"workload\":\"artifact_search_100k_elements_100_notes\",\"sample\":{sample},\"elapsed_ns\":{elapsed_ns}}}"
        );
    }
}
