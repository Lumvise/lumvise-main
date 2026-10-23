//! Indexed prefix and location reads must equal full snapshots across every write path.
use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, ProjectSnapshotScope, ScopedSemanticRead,
    SemanticElement, SemanticOperation, SemanticPartition, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

const PREFIX: &str = "@documents/";

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn element(root: &str, id: &str, path: &str) -> SemanticElement {
    SemanticElement {
        project_root: root.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "prefix-test".into(),
        path: path.into(),
        element_kind: "virtual_document".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn sync(persistence: &LocalPersistence, root: &str, elements: Vec<SemanticElement>) {
    execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: root.into(),
            elements,
            relationships: vec![],
        },
    );
}

fn prefix_ids(persistence: &LocalPersistence, root: &str, inactive: bool) -> Vec<String> {
    let SemanticResult::ScopedGraph(graph) = execute(
        persistence,
        SemanticOperation::ScopedRead(ScopedSemanticRead::PathPrefix {
            project_root: root.into(),
            prefix: PREFIX.into(),
            include_inactive: inactive,
        }),
    ) else {
        panic!("expected a scoped graph")
    };
    assert!(graph.relationships.is_empty());
    graph
        .elements
        .into_iter()
        .map(|element| element.semantic_element_id)
        .collect()
}

/// The oracle: a complete project snapshot filtered by the same prefix.
fn scanned_ids(persistence: &LocalPersistence, root: &str, inactive: bool) -> Vec<String> {
    let SemanticResult::ProjectSnapshot(snapshot) = execute(
        persistence,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::ProjectRoot(root.into()),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected a project snapshot")
    };
    let mut ids: Vec<String> = snapshot
        .elements
        .into_iter()
        .filter(|element| element.path.starts_with(PREFIX))
        .filter(|element| inactive || element.lifecycle == "active")
        .map(|element| element.semantic_element_id)
        .collect();
    ids.sort();
    ids
}

fn assert_matches_scan(persistence: &LocalPersistence, root: &str) -> Vec<String> {
    assert_locations_match_snapshot(persistence, root);
    for inactive in [false, true] {
        assert_eq!(
            prefix_ids(persistence, root, inactive),
            scanned_ids(persistence, root, inactive),
            "include_inactive={inactive}"
        );
    }
    prefix_ids(persistence, root, false)
}

fn assert_locations_match_snapshot(persistence: &LocalPersistence, root: &str) {
    let SemanticResult::ProjectSnapshot(snapshot) = execute(
        persistence,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::ProjectRoot(root.into()),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected project snapshot")
    };
    let paths = snapshot
        .elements
        .iter()
        .map(|element| {
            element
                .path
                .trim()
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_owned()
        })
        .chain([
            "@documents/01".into(),
            "@documents/02".into(),
            "src/moved.rs".into(),
            "src/shared.rs".into(),
        ])
        .collect::<std::collections::BTreeSet<_>>();
    for path in paths {
        for (line, inactive) in [(1, false), (3, false), (8, false), (3, true)] {
            let request = ScopedSemanticRead::Location {
                project_root: root.into(),
                path: Some(path.clone()),
                line,
                include_inactive: inactive,
            };
            let SemanticResult::ScopedGraph(actual) =
                execute(persistence, SemanticOperation::ScopedRead(request.clone()))
            else {
                panic!("expected scoped graph")
            };
            assert_eq!(
                actual,
                snapshot.scoped_graph(&request).unwrap(),
                "path={path}, line={line}, inactive={inactive}"
            );
        }
    }
}

#[test]
fn locations_preserve_normalized_paths_ranges_and_project_isolation_after_writes() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut bounded = element("/a", "bounded", " ./src/shared.rs ");
    bounded.start_line = Some(3);
    bounded.end_line = Some(5);
    let unbounded = element("/a", "unbounded", "/src/shared.rs");
    sync(&persistence, "/a", vec![bounded.clone(), unbounded.clone()]);
    sync(
        &persistence,
        "/b",
        vec![element("/b", "foreign", "src/shared.rs")],
    );
    assert_locations_match_snapshot(&persistence, "/a");
    bounded.start_line = Some(-1);
    bounded.end_line = Some(-1);
    sync(&persistence, "/a", vec![bounded]);
    assert_locations_match_snapshot(&persistence, "/a");
    sync(&persistence, "/a", vec![unbounded]);
    assert_locations_match_snapshot(&persistence, "/a");
}

#[test]
fn selects_by_prefix_inside_one_project_and_honors_include_inactive() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        "/a",
        vec![
            element("/a", "doc-1", "@documents/01"),
            element("/a", "doc-2", "@documents/02"),
            element("/a", "other", "src/@documents/03"),
            element("/a", "close", "@document/04"),
        ],
    );
    sync(
        &persistence,
        "/b",
        vec![element("/b", "foreign", "@documents/09")],
    );
    assert_eq!(assert_matches_scan(&persistence, "/a"), ["doc-1", "doc-2"]);
    assert_eq!(assert_matches_scan(&persistence, "/b"), ["foreign"]);
    sync(
        &persistence,
        "/a",
        vec![
            element("/a", "doc-1", "@documents/01"),
            element("/a", "other", "src/@documents/03"),
            element("/a", "close", "@document/04"),
        ],
    );
    assert_eq!(assert_matches_scan(&persistence, "/a"), ["doc-1"]);
    assert_eq!(prefix_ids(&persistence, "/a", true), ["doc-1", "doc-2"]);
}

#[test]
fn rejects_an_empty_prefix() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let failure = persistence.execute(
        SemanticOperation::ScopedRead(ScopedSemanticRead::PathPrefix {
            project_root: "/a".into(),
            prefix: String::new(),
            include_inactive: false,
        }),
        &InvocationControl::sixty_seconds(),
    );
    assert!(failure.is_err());
}

#[test]
fn stays_consistent_after_upsert_move_remove_partition_and_snapshot_commit() {
    let persistence = LocalPersistence::in_memory().unwrap();
    sync(
        &persistence,
        "/a",
        vec![element("/a", "doc-1", "@documents/01")],
    );
    // Build the index, then mutate it only through writes.
    assert_eq!(assert_matches_scan(&persistence, "/a"), ["doc-1"]);
    sync(
        &persistence,
        "/a",
        vec![
            element("/a", "doc-1", "@documents/01"),
            element("/a", "doc-2", "@documents/02"),
        ],
    );
    assert_eq!(assert_matches_scan(&persistence, "/a"), ["doc-1", "doc-2"]);
    // A moved element leaves its old path and enters the new one.
    sync(
        &persistence,
        "/a",
        vec![
            element("/a", "doc-1", "src/moved.rs"),
            element("/a", "doc-2", "@documents/02"),
        ],
    );
    assert_eq!(assert_matches_scan(&persistence, "/a"), ["doc-2"]);
    execute(
        &persistence,
        SemanticOperation::RemoveElement {
            semantic_element_id: "doc-2".into(),
        },
    );
    assert_eq!(
        assert_matches_scan(&persistence, "/a"),
        Vec::<String>::new()
    );
    execute(
        &persistence,
        SemanticOperation::SyncPartition {
            partition: SemanticPartition {
                provider_instance_id: None,
                project_root: "/a".into(),
                replace_paths: vec!["@documents/03".into()],
            },
            elements: vec![element("/a", "doc-3", "@documents/03")],
            relationships: vec![],
        },
    );
    assert_eq!(assert_matches_scan(&persistence, "/a"), ["doc-3"]);
    execute(
        &persistence,
        SemanticOperation::BeginProjectSnapshot {
            provider_instance_id: None,
            snapshot_id: "s1".into(),
            project_root: "/a".into(),
            partition_paths: vec![],
            page_count: 1,
        },
    );
    execute(
        &persistence,
        SemanticOperation::StageProjectSnapshot {
            snapshot_id: "s1".into(),
            page_index: 0,
            elements: vec![element("/a", "doc-4", "@documents/04")],
            relationships: vec![],
        },
    );
    execute(
        &persistence,
        SemanticOperation::CommitProjectSnapshot {
            snapshot_id: "s1".into(),
        },
    );
    assert_matches_scan(&persistence, "/a");
}

#[test]
fn stays_consistent_after_archive_import_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("source.pz");
    let source = LocalPersistence::in_memory().unwrap();
    sync(
        &source,
        "/repo",
        vec![
            element("/repo", "doc-1", "@documents/01"),
            element("/repo", "doc-2", "@documents/02"),
        ],
    );
    execute(
        &source,
        SemanticOperation::CreatePzSnapshot {
            project_root: "/repo".into(),
            output_path: archive.to_string_lossy().into_owned(),
        },
    );
    let database = directory.path().join("database/lumvise.db");
    let target = LocalPersistence::open(&database).unwrap();
    sync(
        &target,
        "/repo",
        vec![element("/repo", "stale", "@documents/99")],
    );
    assert_eq!(assert_matches_scan(&target, "/repo"), ["stale"]);
    execute(
        &target,
        SemanticOperation::ImportPzSnapshot {
            project_root: "/repo".into(),
            input_path: archive.to_string_lossy().into_owned(),
        },
    );
    assert_eq!(assert_matches_scan(&target, "/repo"), ["doc-1", "doc-2"]);
    drop(target);
    let reopened = LocalPersistence::open(&database).unwrap();
    assert_eq!(assert_matches_scan(&reopened, "/repo"), ["doc-1", "doc-2"]);
}

#[test]
fn the_centralized_codec_carries_path_prefix_reads() {
    // Plugins send this exact JSON through the `storage.semantic` host capability.
    let plugin_request: ScopedSemanticRead = serde_json::from_value(json!({"PathPrefix": {
        "project_root": "/a", "prefix": PREFIX, "include_inactive": true}}))
    .unwrap();
    let request = SemanticOperation::ScopedRead(plugin_request);
    let wire = CentralizedPersistence::encode_semantic_operation(request.clone()).unwrap();
    let decoded = CentralizedPersistence::decode_semantic_operation(&wire).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        serde_json::to_value(decoded).unwrap()
    );
}

#[test]
#[ignore = "benchmark evidence, not a correctness gate: cargo test -p lumvise-db-core --test semantic_path_prefix_read -- --ignored --nocapture"]
fn records_prefix_read_against_a_project_wide_structure_scan() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut elements = vec![element("/big", "doc-1", "@documents/01")];
    elements.push(element("/big", "doc-2", "@documents/02"));
    for folder in 0..200 {
        let parent = format!("folder-{folder}");
        elements.push(element("/big", &parent, &format!("src/{parent}")));
        for child in 0..100 {
            let mut member = element(
                "/big",
                &format!("{parent}-{child}"),
                &format!("src/{parent}/{child}.rs"),
            );
            member.parent_element_id = Some(parent.clone());
            elements.push(member);
        }
    }
    let total = elements.len();
    sync(&persistence, "/big", elements);
    let structure = || {
        let started = std::time::Instant::now();
        let result = execute(
            &persistence,
            SemanticOperation::ScopedRead(ScopedSemanticRead::Structure {
                scope: ProjectSnapshotScope::ProjectRoot("/big".into()),
                max_depth: 0,
                include_inactive: false,
            }),
        );
        let SemanticResult::ScopedGraph(graph) = result else {
            panic!("scoped graph")
        };
        (started.elapsed(), graph.elements.len())
    };
    let (cold_scan, roots) = structure();
    let (warm_scan, _) = structure();
    let started = std::time::Instant::now();
    assert_eq!(prefix_ids(&persistence, "/big", false), ["doc-1", "doc-2"]);
    let first_prefix = started.elapsed();
    let started = std::time::Instant::now();
    prefix_ids(&persistence, "/big", false);
    let warm_prefix = started.elapsed();
    eprintln!(
        "elements={total} structure_roots={roots} structure_scan cold={cold_scan:?} warm={warm_scan:?} \
         path_prefix first(builds index)={first_prefix:?} warm={warm_prefix:?}"
    );
}
