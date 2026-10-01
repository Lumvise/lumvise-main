use lumvise_db_core::{
    CentralizedPersistence, LocalPersistence, ProjectSnapshotScope, ScopedSemanticRead,
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult, SemanticScopedGraph,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn tree_element(id: &str, parent: Option<&str>) -> SemanticElement {
    SemanticElement {
        project_root: "/scoped".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "scoped-test".into(),
        path: format!("src/{id}.rs"),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: parent.map(str::to_owned),
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"retained": true}),
    }
}

fn seed(persistence: &LocalPersistence) {
    execute(
        persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![
                tree_element("root", None),
                tree_element("child", Some("root")),
                tree_element("grandchild", Some("child")),
                tree_element("other", None),
            ],
            relationships: vec![],
        },
    );
}

fn scoped(
    persistence: &LocalPersistence,
    scope: ProjectSnapshotScope,
    max_depth: usize,
) -> SemanticScopedGraph {
    let result = execute(
        persistence,
        SemanticOperation::ScopedRead(ScopedSemanticRead::Structure {
            scope,
            max_depth,
            include_inactive: false,
        }),
    );
    match result {
        SemanticResult::ScopedGraph(graph) => graph,
        other => panic!("expected ScopedGraph, got {other:?}"),
    }
}

#[test]
fn structure_reads_limit_depth_keep_parent_edges_and_share_full_snapshot_revision() {
    let persistence = LocalPersistence::in_memory().unwrap();
    seed(&persistence);
    let leaf = scoped(
        &persistence,
        ProjectSnapshotScope::SemanticElement("child".into()),
        0,
    );
    assert_eq!(leaf.elements, [tree_element("child", Some("root"))]);
    assert_eq!(leaf.relationships.len(), 1);
    assert_eq!(leaf.relationships[0].source_element_id, "root");
    let tree = scoped(
        &persistence,
        ProjectSnapshotScope::SemanticElement("root".into()),
        1,
    );
    assert_eq!(
        tree.elements
            .iter()
            .map(|element| element.name.as_str())
            .collect::<Vec<_>>(),
        ["child", "root"]
    );
    let full = execute(
        &persistence,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::SemanticElement("child".into()),
            artifact_namespace: None,
        },
    );
    let SemanticResult::ProjectSnapshot(full) = full else {
        panic!("expected complete snapshot")
    };
    assert_eq!(full.elements.len(), 4);
    assert_eq!(
        (leaf.commit_version, leaf.published_at),
        (full.commit_version, full.published_at)
    );
}

#[test]
fn project_structure_roots_and_scoped_codec_round_trip() {
    let persistence = LocalPersistence::in_memory().unwrap();
    seed(&persistence);
    let roots = scoped(
        &persistence,
        ProjectSnapshotScope::ProjectRoot("/scoped".into()),
        0,
    );
    assert_eq!(
        roots
            .elements
            .iter()
            .map(|element| element.name.as_str())
            .collect::<Vec<_>>(),
        ["other", "root"]
    );
    let request = SemanticOperation::ScopedRead(ScopedSemanticRead::Structure {
        scope: ProjectSnapshotScope::SemanticElement("root".into()),
        max_depth: 1,
        include_inactive: false,
    });
    let wire = CentralizedPersistence::encode_semantic_operation(request.clone()).unwrap();
    let decoded = CentralizedPersistence::decode_semantic_operation(&wire).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        serde_json::to_value(decoded).unwrap()
    );
    let wire =
        CentralizedPersistence::encode_semantic_result(SemanticResult::ScopedGraph(roots.clone()))
            .unwrap();
    let SemanticResult::ScopedGraph(decoded) =
        CentralizedPersistence::decode_semantic_result(&wire).unwrap()
    else {
        panic!("expected scoped graph")
    };
    assert_eq!(decoded, roots);
}

#[test]
fn scoped_structure_rejects_missing_roots_and_out_of_range_depth() {
    let persistence = LocalPersistence::in_memory().unwrap();
    seed(&persistence);
    for (id, depth, expected) in [
        ("missing", 0, "existing semantic element"),
        ("root", 129, "0 through 128"),
    ] {
        let error = persistence
            .execute(
                SemanticOperation::ScopedRead(ScopedSemanticRead::Structure {
                    scope: ProjectSnapshotScope::SemanticElement(id.into()),
                    max_depth: depth,
                    include_inactive: false,
                }),
                &InvocationControl::sixty_seconds(),
            )
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn project_roots_preserve_inactive_parents_cycles_and_nonstructural_edges() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut inactive = tree_element("inactive", None);
    inactive.lifecycle = "inactive".into();
    let mut labeled = dependency_edge("root", "labeled-child");
    labeled.label = "contains".into();
    labeled.metadata = json!({"large": "x".repeat(65536)});
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![
                tree_element("root", None),
                inactive,
                tree_element("orphan", Some("inactive")),
                tree_element("unrelated", None),
                tree_element("labeled-child", None),
                tree_element("cycle-a", Some("cycle-b")),
                tree_element("cycle-b", Some("cycle-a")),
            ],
            relationships: vec![labeled, dependency_edge("root", "unrelated")],
        },
    );
    let roots = scoped(
        &persistence,
        ProjectSnapshotScope::ProjectRoot("/scoped".into()),
        0,
    );
    assert_eq!(selected_ids(&roots), ["orphan", "root", "unrelated"]);
    let SemanticResult::ScopedGraph(including_inactive) = execute(
        &persistence,
        SemanticOperation::ScopedRead(ScopedSemanticRead::Structure {
            scope: ProjectSnapshotScope::ProjectRoot("/scoped".into()),
            max_depth: 0,
            include_inactive: true,
        }),
    ) else {
        panic!("expected scoped graph including inactive roots");
    };
    assert_eq!(
        selected_ids(&including_inactive),
        ["inactive", "root", "unrelated"]
    );
}

#[test]
#[ignore = "release scoped-read evidence; run with --release --ignored --nocapture"]
fn depth_zero_tree_in_ten_thousand_element_project() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut elements = vec![tree_element("root", None)];
    elements.extend((0..10_000).map(|index| tree_element(&format!("child{index}"), Some("root"))));
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements,
            relationships: vec![],
        },
    );
    for sample in 0..3 {
        let started = std::time::Instant::now();
        let graph = scoped(
            &persistence,
            ProjectSnapshotScope::SemanticElement("root".into()),
            0,
        );
        let elapsed_ns = started.elapsed().as_nanos();
        assert_eq!(graph.elements.len(), 1);
        assert!(graph.relationships.is_empty());
        eprintln!(
            "{{\"workload\":\"depth_zero_tree_10k_children\",\"sample\":{sample},\"elapsed_ns\":{elapsed_ns}}}"
        );
    }
    for sample in 0..3 {
        let started = std::time::Instant::now();
        let found = location(&persistence, Some("src/child9999.rs"), 1, false);
        let elapsed_ns = started.elapsed().as_nanos();
        assert_eq!(found.elements.len(), 1);
        assert_eq!(found.elements[0].semantic_element_id, "child9999");
        eprintln!(
            "{{\"workload\":\"location_10k_files\",\"sample\":{sample},\"elapsed_ns\":{elapsed_ns}}}"
        );
    }
}

fn location(
    persistence: &LocalPersistence,
    path: Option<&str>,
    line: i64,
    include_inactive: bool,
) -> SemanticScopedGraph {
    let request = ScopedSemanticRead::Location {
        project_root: "/scoped".into(),
        path: path.map(str::to_owned),
        line,
        include_inactive,
    };
    let wire =
        CentralizedPersistence::encode_semantic_operation(SemanticOperation::ScopedRead(request))
            .unwrap();
    let result = execute(
        persistence,
        CentralizedPersistence::decode_semantic_operation(&wire).unwrap(),
    );
    let SemanticResult::ScopedGraph(graph) = result else {
        panic!("expected scoped graph")
    };
    graph
}

#[test]
fn location_selects_matching_ranges_and_ancestor_context_without_other_files() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut child = tree_element("child", Some("root"));
    child.path = " ./src/shared.rs ".into();
    child.end_line = Some(20);
    let mut nested = tree_element("nested", Some("child"));
    nested.path = "src/shared.rs".into();
    nested.start_line = Some(10);
    nested.end_line = Some(12);
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![
                tree_element("root", None),
                child.clone(),
                nested.clone(),
                tree_element("other", None),
            ],
            relationships: vec![],
        },
    );
    let mut foreign = tree_element("foreign", None);
    foreign.project_root = "/other-project".into();
    foreign.path = "src/shared.rs".into();
    foreign.end_line = Some(20);
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/other-project".into(),
            elements: vec![foreign],
            relationships: vec![],
        },
    );
    let selected = location(&persistence, Some("src/shared.rs"), 11, false);
    assert_eq!(selected.elements, [child.clone(), nested.clone()]);
    assert_eq!(selected.relationships.len(), 2);
    assert_eq!(
        location(&persistence, Some("src/shared.rs"), 5, false).elements,
        [child.clone()]
    );
    let outside = location(&persistence, None, 11, false);
    assert!(outside.elements.is_empty() && outside.relationships.is_empty());
    assert_eq!(outside.commit_version, selected.commit_version);
    child.path = "src/moved.rs".into();
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![tree_element("root", None), child.clone()],
            relationships: vec![],
        },
    );
    assert!(
        location(&persistence, Some("src/shared.rs"), 11, false)
            .elements
            .is_empty()
    );
    let inactive = location(&persistence, Some("src/shared.rs"), 11, true);
    assert_eq!(inactive.elements.len(), 1);
    assert_eq!(inactive.elements[0].semantic_element_id, "nested");
    assert_eq!(inactive.elements[0].lifecycle, "inactive");
    assert_eq!(
        location(&persistence, Some("src/moved.rs"), 11, false).elements,
        [child]
    );
}

fn dependency(
    persistence: &LocalPersistence,
    direction: lumvise_db_core::SemanticDependencyDirection,
    max_depth: usize,
    include_descendants: bool,
) -> SemanticScopedGraph {
    let request = SemanticOperation::ScopedRead(ScopedSemanticRead::Dependency {
        semantic_element_id: "root".into(),
        direction,
        max_depth,
        include_descendants,
        include_inactive: false,
    });
    let wire = CentralizedPersistence::encode_semantic_operation(request).unwrap();
    let request = CentralizedPersistence::decode_semantic_operation(&wire).unwrap();
    let wire =
        CentralizedPersistence::encode_semantic_result(execute(persistence, request)).unwrap();
    match CentralizedPersistence::decode_semantic_result(&wire).unwrap() {
        SemanticResult::ScopedGraph(graph) => graph,
        other => panic!("expected ScopedGraph, got {other:?}"),
    }
}

fn dependency_edge(source: &str, target: &str) -> lumvise_db_core::SemanticRelationship {
    lumvise_db_core::SemanticRelationship {
        project_root: "/scoped".into(),
        source_element_id: source.into(),
        target_element_id: target.into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({"weight": 2}),
    }
}

fn selected_ids(graph: &SemanticScopedGraph) -> Vec<&str> {
    graph
        .elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect()
}

#[test]
fn dependency_limits_hydration_preserves_scope_count_direction_descendants_and_cycles() {
    use lumvise_db_core::SemanticDependencyDirection::{Both, Dependencies, Dependents};
    let persistence = LocalPersistence::in_memory().unwrap();
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![
                tree_element("root", None),
                tree_element("child", Some("root")),
                tree_element("a", None),
                tree_element("b", None),
                tree_element("incoming", None),
                tree_element("unrelated", None),
            ],
            relationships: vec![
                dependency_edge("child", "a"),
                dependency_edge("a", "b"),
                dependency_edge("b", "a"),
                dependency_edge("incoming", "root"),
            ],
        },
    );
    let zero = dependency(&persistence, Both, 0, true);
    assert_eq!(selected_ids(&zero), ["root"]);
    assert!(zero.relationships.is_empty());
    assert_eq!(zero.scope_element_count, Some(6));
    let one = dependency(&persistence, Dependencies, 1, true);
    assert_eq!(selected_ids(&one), ["a", "child", "root"]);
    assert_eq!(one.scope_element_count, Some(6));
    assert_eq!(one.relationships.len(), 2);
    let two = dependency(&persistence, Dependencies, 2, true);
    assert_eq!(selected_ids(&two), ["a", "b", "child", "root"]);
    assert!(
        two.relationships
            .iter()
            .any(|edge| edge.source_element_id == "b" && edge.target_element_id == "a")
    );
    let incoming = dependency(&persistence, Dependents, 1, true);
    assert_eq!(selected_ids(&incoming), ["incoming", "root"]);
    let root_only = dependency(&persistence, Both, 1, false);
    assert_eq!(selected_ids(&root_only), ["child", "incoming", "root"]);
    assert_eq!(root_only.scope_element_count, Some(1));
    assert_eq!(root_only.relationships.len(), 2);
    assert_eq!(
        (zero.commit_version, zero.published_at),
        (two.commit_version, two.published_at)
    );
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![tree_element("root", None), tree_element("new", None)],
            relationships: vec![],
        },
    );
    let refreshed = dependency(&persistence, Both, 0, true);
    assert_eq!(refreshed.scope_element_count, Some(2));
    assert!(refreshed.commit_version > root_only.commit_version);
    let error = persistence
        .execute(
            SemanticOperation::ScopedRead(ScopedSemanticRead::Dependency {
                semantic_element_id: "root".into(),
                direction: Both,
                max_depth: 129,
                include_descendants: true,
                include_inactive: false,
            }),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("0 through 128"), "{error}");
}

#[test]
fn selective_subgraph_deduplicates_containment_and_keeps_touching_links() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let relationships = [
        ("grandchild", "root", "contains"),
        ("other", "child", "uses"),
    ]
    .into_iter()
    .map(
        |(source, target, kind)| lumvise_db_core::SemanticRelationship {
            project_root: "/scoped".into(),
            source_element_id: source.into(),
            target_element_id: target.into(),
            relationship_kind: kind.into(),
            label: kind.into(),
            metadata: json!({}),
        },
    )
    .collect();
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: vec![
                tree_element("root", None),
                tree_element("child", Some("root")),
                tree_element("grandchild", Some("child")),
                tree_element("other", None),
            ],
            relationships,
        },
    );
    let SemanticResult::SelectiveSubgraph(Some(graph)) = execute(
        &persistence,
        SemanticOperation::SelectiveSubgraph {
            project_root: "/scoped".into(),
            root_element_id: "root".into(),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected selective subgraph")
    };
    assert_eq!(
        graph
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        ["child", "grandchild", "root"]
    );
    assert!(
        graph
            .elements
            .iter()
            .all(|element| element.metadata["retained"] == true)
    );
    assert_eq!(graph.external_elements, [tree_element("other", None)]);
    assert!(
        graph
            .relationships
            .iter()
            .any(|edge| edge.relationship_kind == "uses")
    );
}

#[test]
fn batched_subtree_preserves_metadata_and_stops_at_inactive_parents() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut elements = vec![tree_element("root", None)];
    for index in 0..32 {
        let parent = if index == 0 {
            "root".into()
        } else {
            format!("child-{}", index - 1)
        };
        let mut child = tree_element(&format!("child-{index}"), Some(&parent));
        child.metadata["text"] = json!("x".repeat(8192));
        elements.push(child);
    }
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements,
            relationships: vec![],
        },
    );
    execute(
        &persistence,
        SemanticOperation::RemoveElement {
            semantic_element_id: "child-16".into(),
        },
    );
    let SemanticResult::SelectiveSubgraph(Some(graph)) = execute(
        &persistence,
        SemanticOperation::SelectiveSubgraph {
            project_root: "/scoped".into(),
            root_element_id: "root".into(),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected selective subgraph")
    };
    assert_eq!(graph.elements.len(), 17);
    assert!(
        graph
            .elements
            .iter()
            .filter(|element| element.semantic_element_id != "root")
            .all(|element| element.metadata["text"].as_str().unwrap().len() == 8192)
    );
}

#[test]
fn batched_touching_relationships_preserve_metadata_directions_and_deduplication() {
    let persistence = LocalPersistence::in_memory().unwrap();
    let mut expected = vec![
        dependency_edge("child", "other"),
        dependency_edge("root", "child"),
    ];
    expected[0].metadata = json!({"body": "x".repeat(65536)});
    let mut relationships = expected.clone();
    relationships.push(dependency_edge("other", "unrelated"));
    execute(
        &persistence,
        SemanticOperation::SyncStructure {
            project_root: "/scoped".into(),
            elements: ["root", "child", "other", "unrelated"]
                .map(|id| tree_element(id, None))
                .to_vec(),
            relationships,
        },
    );
    for ids in [vec!["child"], vec!["root", "child", "missing"]] {
        let SemanticResult::Relationships(found) = execute(
            &persistence,
            SemanticOperation::RelationshipsTouchingElements {
                semantic_element_ids: ids.into_iter().map(str::to_owned).collect(),
            },
        ) else {
            panic!("expected touching relationships");
        };
        assert_eq!(found, expected);
    }
    execute(
        &persistence,
        SemanticOperation::RemoveElement {
            semantic_element_id: "child".into(),
        },
    );
    let SemanticResult::Relationships(found) = execute(
        &persistence,
        SemanticOperation::RelationshipsTouchingElements {
            semantic_element_ids: ["child".into(), "missing".into()].into_iter().collect(),
        },
    ) else {
        panic!("expected relationships after removal");
    };
    assert!(found.is_empty());
}
