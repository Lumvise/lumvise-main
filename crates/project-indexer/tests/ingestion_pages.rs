//! Both real producers use the same lazy serializer, including its revision-observation boundary.
use lumvise_contracts::*;
use lumvise_project_indexer::SemanticIndexPages;
use serde_json::{Value, json};

fn batch(elements: usize, relationships: usize) -> IndexBatchRequest {
    IndexBatchRequest {
        provider_instance_id: "source".into(),
        project_root: "/repo".into(),
        revision_observation: Some(SemanticIndexObservation {
            project_id: "lineage".into(),
            selection_id: "working".into(),
            source_context: "local".into(),
            producer_base: Some(SemanticProducerBase {
                selection_id: "producer:checkout".into(),
                expected_generation: i64::MAX,
            }),
            expected: SemanticSelectionExpectation {
                artifact_edits: Default::default(),
                generation: i64::MAX,
                records: (0..20001)
                    .map(|id| SemanticRecordRef::element(format!("old-{id}")))
                    .collect(),
                connection_ids: vec!["old-edge".into()],
            },
        }),
        replace_paths: vec!["src/changed.rs".into(), "src/deleted.rs".into()],
        removed_paths: vec!["src/deleted.rs".into()],
        semantic_sources: vec![SemanticSourceUpsert {
            semantic_source_id: "logical-source".into(),
            kind: "filesystem".into(),
            name: "source".into(),
            root_path: None,
            root_uri: "lumvise://source".into(),
        }],
        semantic_elements: (0..elements).map(element).collect(),
        semantic_relationships: (0..relationships)
            .map(|_| SemanticRelationshipUpsert {
                source_element_id: "file-0".into(),
                target_element_id: Some("file-1".into()),
                relationship_kind: "semantic".into(),
                relationship_label: "calls".into(),
                target_label: None,
                target_locator: None,
            })
            .collect(),
    }
}

fn element(index: usize) -> SemanticElementUpsert {
    SemanticElementUpsert {
        revision_dependencies: Vec::new(),
        semantic_source_id: "logical-source".into(),
        semantic_element_id: format!("file-{index}"),
        path: format!("src/{index}.rs"),
        semantic_element_type: "file".into(),
        semantic_element_name: index.to_string(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        metadata: Some(json!({"n": index})),
    }
}

fn values(batch: &IndexBatchRequest, full: bool) -> Vec<Value> {
    SemanticIndexPages::new(batch, "frozen-operation", full)
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_large_revision_observation_and_source_descriptor_travel_only_on_page_zero() {
    let batch = batch(2501, 10001);
    let pages = values(&batch, false);
    assert_eq!(pages.len(), 3);
    assert_eq!(
        pages[0]["revision_observation"],
        serde_json::to_value(&batch.revision_observation).unwrap()
    );
    assert_eq!(
        pages
            .iter()
            .map(|page| page["revision_observation"]["expected"]["records"]
                .as_array()
                .map_or(0, Vec::len))
            .sum::<usize>(),
        20001
    );
    for (index, page) in pages.iter().enumerate() {
        assert_eq!(page["ingestion_page_index"], index);
        assert_eq!(page["ingestion_page_count"], 3);
        assert_eq!(page["ingestion_job_id"], "frozen-operation");
        assert_eq!(page["provider_instance_id"], "source");
        assert_eq!(page["project_root"], "/repo");
        if index == 0 {
            continue;
        }
        assert!(page["revision_observation"].is_null());
        assert_eq!(page["semantic_sources"], json!([]));
        assert_eq!(page["removed_paths"], json!([]));
    }
    assert_eq!(
        pages
            .iter()
            .map(|page| page["semantic_elements"].as_array().unwrap().len())
            .sum::<usize>(),
        2501
    );
    assert_eq!(
        pages
            .iter()
            .map(|page| page["semantic_relationships"].as_array().unwrap().len())
            .sum::<usize>(),
        10001
    );
}

#[test]
fn partial_page_zero_keeps_deleted_paths_and_later_pages_cover_only_their_elements() {
    let batch = batch(1001, 10001);
    let pages = values(&batch, false);
    assert_eq!(pages[0]["replace_paths"], json!(batch.replace_paths));
    assert_eq!(pages[0]["removed_paths"], json!(batch.removed_paths));
    assert_eq!(pages[1]["replace_paths"], json!(["src/1000.rs"]));
    assert_eq!(pages[2]["replace_paths"], json!([]));
    assert_eq!(pages[2]["semantic_elements"], json!([]));
    assert!(
        values(&batch, true)
            .iter()
            .all(|page| page["replace_paths"] == json!([]))
    );
}

#[test]
fn an_empty_authoritative_replacement_still_has_one_page_and_exact_iterator_length() {
    let batch = batch(0, 0);
    let mut pages = SemanticIndexPages::new(&batch, "delete-scope", false);
    assert_eq!(pages.len(), 1);
    let page = serde_json::to_value(pages.next().unwrap()).unwrap();
    assert_eq!(page["removed_paths"], json!(batch.removed_paths));
    assert_eq!(pages.len(), 0);
    assert!(pages.next().is_none());
    assert!(pages.next().is_none());
}
