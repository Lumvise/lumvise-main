use lumvise_db_core::{
    DbCore, SemanticElement, SemanticGraphGranularity, SemanticGraphProjectionRequest,
    SemanticPartition, SemanticRelationship,
};
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[test]
fn semantic_snapshot_sync_replaces_ten_thousand_elements() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let first_batch = semantic_batch("stable", 10_000);
    storage
        .sync_semantic_structure("/repo", &first_batch.elements, &first_batch.relationships)
        .unwrap();

    let second_batch = semantic_batch("renamed", 10_000);
    storage
        .sync_semantic_structure("/repo", &second_batch.elements, &second_batch.relationships)
        .unwrap();

    let stored = storage.elements_for_project("/repo").unwrap();
    assert_eq!(stored.len(), 10_000);
    assert!(
        stored
            .iter()
            .all(|element| element.name.starts_with("item_"))
    );
}

#[test]
fn sparse_relationship_update_preserves_unmodified_large_project_edges() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let batch = semantic_batch("stable", 10_000);
    storage
        .sync_semantic_structure("/repo", &batch.elements, &batch.relationships)
        .unwrap();
    let mut relationships = batch.relationships;
    relationships[5_000].metadata = json!({"revision": 2});

    storage
        .sync_semantic_structure("/repo", &batch.elements, &relationships)
        .unwrap();

    let updated = storage.relationships_from("stable:element:5000").unwrap();
    assert_eq!(updated[0].metadata["revision"], json!(2));
}

#[test]
fn semantic_partition_sync_updates_only_the_requested_partition() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let first_batch = semantic_batch("stable", 10_000);
    storage
        .sync_semantic_structure("/repo", &first_batch.elements, &first_batch.relationships)
        .unwrap();

    let partition = SemanticPartition {
        project_root: "/repo".to_string(),
        replace_paths: vec!["src/module_42.rs".to_string()],
    };
    let update = semantic_partition_batch_revision("stable", 42, 100, 2);
    storage
        .sync_semantic_partition(&partition, &update.elements, &update.relationships)
        .unwrap();

    let updated = storage.element("stable:element:4200").unwrap().unwrap();
    let untouched = storage.element("stable:element:4100").unwrap().unwrap();
    assert_eq!(updated.metadata["revision"], json!(2));
    assert_eq!(untouched.metadata["revision"], json!(null));
}

#[test]
fn semantic_subtree_read_returns_only_the_requested_branch() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let batch = semantic_batch("stable", 12);
    storage
        .sync_semantic_structure("/repo", &batch.elements, &batch.relationships)
        .unwrap();

    let subtree = storage.elements_in_subtree("stable:element:8").unwrap();
    let element_ids = subtree
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        element_ids,
        vec![
            "stable:element:8",
            "stable:element:9",
            "stable:element:10",
            "stable:element:11"
        ]
    );
}

#[test]
fn scoped_relationship_read_returns_incoming_and_outgoing_edges() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let batch = semantic_batch("stable", 12);
    storage
        .sync_semantic_structure("/repo", &batch.elements, &batch.relationships)
        .unwrap();
    let requested = HashSet::from(["stable:element:8".to_string()]);

    let relationships = storage.relationships_touching_elements(&requested).unwrap();
    let endpoints = relationships
        .iter()
        .map(|relationship| {
            (
                relationship.source_element_id.as_str(),
                relationship.target_element_id.as_str(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        endpoints,
        vec![
            ("stable:element:7", "stable:element:8"),
            ("stable:element:8", "stable:element:9")
        ]
    );
}

#[test]
fn semantic_subtree_read_terminates_when_parent_links_form_a_cycle() {
    let db = DbCore::in_memory().unwrap();
    let storage = db.storage_manager().semantic_storage();
    let mut root = indexed_element("stable", 0);
    let child = indexed_element("stable", 1);
    storage.upsert_element(&root).unwrap();
    storage.upsert_element(&child).unwrap();
    root.parent_element_id = Some(child.semantic_element_id.clone());
    storage.upsert_element(&root).unwrap();

    let subtree = storage
        .elements_in_subtree(&root.semantic_element_id)
        .unwrap();

    assert_eq!(subtree.len(), 2);
}

#[test]
fn persistent_semantic_partition_sync_survives_reopen() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let db = DbCore::open(temp.path()).unwrap();
    let storage = db.storage_manager().semantic_storage();
    let first_batch = semantic_batch("stable", 10_000);
    storage
        .sync_semantic_structure("/repo", &first_batch.elements, &first_batch.relationships)
        .unwrap();

    let partition = SemanticPartition {
        project_root: "/repo".to_string(),
        replace_paths: vec!["src/module_42.rs".to_string()],
    };
    let update = semantic_partition_batch_revision("stable", 42, 100, 2);
    storage
        .sync_semantic_partition(&partition, &update.elements, &update.relationships)
        .unwrap();
    drop(db);

    let reopened = DbCore::open(temp.path()).unwrap();
    let relationships = reopened
        .storage_manager()
        .semantic_storage()
        .relationships_from("stable:element:4200")
        .unwrap();
    assert_eq!(relationships[0].metadata["revision"], json!(2));
}

#[test]
#[ignore = "realistic 30k/60k persisted performance fixture"]
fn large_file_projection_meets_interactive_budget() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let fixture = large_projection_fixture();
    assert_eq!(fixture.elements.len(), 30_752);
    assert_eq!(fixture.relationships.len(), 61_230);
    {
        let db = DbCore::open(temp.path()).unwrap();
        let storage = db.storage_manager().semantic_storage();
        storage
            .sync_semantic_structure("/large-repo", &fixture.elements, &fixture.relationships)
            .unwrap();
    }
    let db = DbCore::open(temp.path()).unwrap();
    let storage = db.storage_manager().semantic_storage();

    let overview_request = SemanticGraphProjectionRequest {
        project_root: "/large-repo".into(),
        target_path: None,
        granularity: SemanticGraphGranularity::File,
        recursive: true,
        include_external: false,
        include_first_neighbors: false,
    };
    let cold_started = Instant::now();
    let overview = storage.project_renderer_graph(&overview_request).unwrap();
    let cold_elapsed = cold_started.elapsed();
    assert_eq!(overview.nodes.len(), 826);
    assert_eq!(overview.edges.len(), 3_330);
    assert_eq!(
        overview.edges.iter().map(|edge| edge.weight).sum::<u64>(),
        31_304
    );
    assert!(
        cold_elapsed < Duration::from_secs(1),
        "cold file overview exceeded one second: {cold_elapsed:?}"
    );

    let cached_started = Instant::now();
    let cached = storage.project_renderer_graph(&overview_request).unwrap();
    let cached_elapsed = cached_started.elapsed();
    assert_eq!(cached, overview);
    assert!(
        cached_elapsed < Duration::from_millis(250),
        "cached file overview exceeded 250ms: {cached_elapsed:?}"
    );

    let drill_started = Instant::now();
    let drill = storage
        .project_renderer_graph(&SemanticGraphProjectionRequest {
            target_path: Some("src/module_0.rs".into()),
            include_first_neighbors: true,
            ..overview_request
        })
        .unwrap();
    let drill_elapsed = drill_started.elapsed();
    assert!(drill.nodes.iter().any(|node| node.id == "file:0"));
    assert!(!drill.edges.is_empty());
    assert!(
        drill_elapsed < Duration::from_secs(1),
        "file drill exceeded one second: {drill_elapsed:?}"
    );
    eprintln!(
        "large projection timings: cold={cold_elapsed:?} cached={cached_elapsed:?} drill={drill_elapsed:?}"
    );
}

fn large_projection_fixture() -> SemanticBatchFixture {
    const FILES: usize = 826;
    const EXTRA_FUNCTIONS: usize = 190;
    const PROJECTED_EDGES: usize = 3_330;
    const CALLS: usize = 31_304;
    let mut elements = Vec::with_capacity(30_752);
    let mut relationships = Vec::with_capacity(61_230);
    let mut functions_by_file = Vec::with_capacity(FILES);
    for file_index in 0..FILES {
        let file_id = format!("file:{file_index}");
        let path = format!("src/module_{file_index}.rs");
        elements.push(large_element(&file_id, &path, "file", None, file_index));
        let function_count = 36 + usize::from(file_index < EXTRA_FUNCTIONS);
        let mut function_ids = Vec::with_capacity(function_count);
        for function_index in 0..function_count {
            let function_id = format!("function:{file_index}:{function_index}");
            elements.push(large_element(
                &function_id,
                &path,
                "function",
                Some(file_id.clone()),
                function_index,
            ));
            relationships.push(large_relationship(
                &file_id,
                &function_id,
                "contains",
                "contains",
            ));
            function_ids.push(function_id);
        }
        functions_by_file.push(function_ids);
    }
    let mut pair_index = 0;
    'pairs: for source_file in 0..FILES {
        for offset in 1..FILES {
            if pair_index == PROJECTED_EDGES {
                break 'pairs;
            }
            let target_file = (source_file + offset) % FILES;
            let calls_for_pair = 9 + usize::from(pair_index < CALLS - PROJECTED_EDGES * 9);
            for call_index in 0..calls_for_pair {
                let sources = &functions_by_file[source_file];
                let targets = &functions_by_file[target_file];
                relationships.push(large_relationship(
                    &sources[call_index % sources.len()],
                    &targets[(call_index * 7 + pair_index) % targets.len()],
                    "semantic",
                    "calls",
                ));
            }
            pair_index += 1;
        }
    }
    SemanticBatchFixture {
        elements,
        relationships,
    }
}

fn large_element(
    id: &str,
    path: &str,
    kind: &str,
    parent_element_id: Option<String>,
    line: usize,
) -> SemanticElement {
    SemanticElement {
        project_root: "/large-repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "large-fixture".into(),
        path: path.into(),
        element_kind: kind.into(),
        name: id.into(),
        parent_element_id,
        content_fingerprint: Some(format!("fp1:{line:016x}:{id}")),
        start_line: Some(line as i64 + 1),
        end_line: Some(line as i64 + 2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn large_relationship(
    source: &str,
    target: &str,
    relationship_kind: &str,
    label: &str,
) -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/large-repo".into(),
        source_element_id: source.into(),
        target_element_id: target.into(),
        relationship_kind: relationship_kind.into(),
        label: label.into(),
        metadata: json!({}),
    }
}

struct SemanticBatchFixture {
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
}

fn semantic_batch(id_prefix: &str, count: usize) -> SemanticBatchFixture {
    let mut elements = Vec::with_capacity(count);
    let mut relationships = Vec::with_capacity(count.saturating_sub(1));
    for index in 0..count {
        elements.push(indexed_element(id_prefix, index));
        if index > 0 {
            relationships.push(contains_relationship(id_prefix, index - 1, index));
        }
    }
    SemanticBatchFixture {
        elements,
        relationships,
    }
}

fn semantic_partition_batch(
    id_prefix: &str,
    module_index: usize,
    count: usize,
) -> SemanticBatchFixture {
    let start = module_index * count;
    let mut elements = Vec::with_capacity(count);
    let mut relationships = Vec::with_capacity(count.saturating_sub(1));
    for offset in 0..count {
        let index = start + offset;
        elements.push(indexed_element(id_prefix, index));
        if offset > 0 {
            relationships.push(contains_relationship(id_prefix, index - 1, index));
        }
    }
    SemanticBatchFixture {
        elements,
        relationships,
    }
}

fn semantic_partition_batch_revision(
    id_prefix: &str,
    module_index: usize,
    count: usize,
    revision: i64,
) -> SemanticBatchFixture {
    let mut fixture = semantic_partition_batch(id_prefix, module_index, count);
    for element in &mut fixture.elements {
        element.metadata = json!({"index": element.name, "revision": revision});
    }
    for relationship in &mut fixture.relationships {
        relationship.metadata = json!({"revision": revision});
    }
    fixture
}

fn indexed_element(id_prefix: &str, index: usize) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: format!("{id_prefix}:element:{index}"),
        semantic_source_id: "source".to_string(),
        path: format!("src/module_{}.rs", index / 100),
        element_kind: element_kind(index).to_string(),
        name: format!("item_{index}"),
        parent_element_id: parent_id(id_prefix, index),
        content_fingerprint: Some(format!("fp1:{index:016x}:item-body-{index}")),
        start_line: Some((index as i64) + 1),
        end_line: Some((index as i64) + 2),
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({"index": index}),
    }
}

fn contains_relationship(id_prefix: &str, source: usize, target: usize) -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/repo".to_string(),
        source_element_id: format!("{id_prefix}:element:{source}"),
        target_element_id: format!("{id_prefix}:element:{target}"),
        relationship_kind: "contains".to_string(),
        label: "contains".to_string(),
        metadata: json!({}),
    }
}

fn parent_id(id_prefix: &str, index: usize) -> Option<String> {
    (index > 0).then(|| format!("{id_prefix}:element:{}", index - 1))
}

fn element_kind(index: usize) -> &'static str {
    match index % 5 {
        0 => "file",
        1 => "class",
        2 => "method",
        3 => "property",
        _ => "function",
    }
}
