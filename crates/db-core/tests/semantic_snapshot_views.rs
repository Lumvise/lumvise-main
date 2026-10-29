use lumvise_db_core::{
    LocalPersistence, ProjectSnapshotScope, ScopedSemanticRead, SemanticDependencyDirection,
    SemanticElement, SemanticGraphGranularity, SemanticGraphProjectionRequest, SemanticOperation,
    SemanticPersistence, SemanticProjectSnapshot, SemanticRelationship, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;

fn execute(local: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    local
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}
fn publication() -> (LocalPersistence, SemanticProjectSnapshot) {
    let local = LocalPersistence::in_memory().unwrap();
    execute(
        &local,
        SemanticOperation::SyncStructure {
            project_root: "/project".into(),
            elements: vec![element("parent", "file"), element("child", "function")],
            relationships: vec![SemanticRelationship {
                project_root: "/project".into(),
                source_element_id: "parent".into(),
                target_element_id: "child".into(),
                relationship_kind: "contains".into(),
                label: "contains".into(),
                metadata: serde_json::json!({}),
            }],
        },
    );
    let SemanticResult::ProjectSnapshot(snapshot) = execute(
        &local,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::ProjectRoot("/project".into()),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected snapshot");
    };
    (local, snapshot)
}
fn element(id: &str, kind: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: "/project".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "test".into(),
        path: "src/file.rs".into(),
        element_kind: kind.into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(8),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
    }
}
#[test]
fn portable_snapshot_renderer_uses_local_projection_rules_and_publication() {
    let (local, snapshot) = publication();
    for granularity in [
        SemanticGraphGranularity::File,
        SemanticGraphGranularity::Property,
    ] {
        let request = SemanticGraphProjectionRequest {
            project_root: "/project".into(),
            target_path: None,
            granularity,
            recursive: true,
            include_external: false,
            include_first_neighbors: false,
        };
        let SemanticResult::RendererGraphProjection(expected) = execute(
            &local,
            SemanticOperation::ProjectRendererGraph(request.clone()),
        ) else {
            panic!("expected renderer graph");
        };
        assert_eq!(snapshot.renderer_graph(&request).unwrap(), expected);
    }
}
#[test]
fn portable_snapshot_scoped_reads_share_local_selection_and_reject_other_roots() {
    let (local, snapshot) = publication();
    for request in [
        ScopedSemanticRead::Structure {
            scope: ProjectSnapshotScope::SemanticElement("child".into()),
            max_depth: 0,
            include_inactive: false,
        },
        ScopedSemanticRead::Location {
            project_root: "/project".into(),
            path: Some("src/file.rs".into()),
            line: 3,
            include_inactive: false,
        },
        ScopedSemanticRead::Dependency {
            semantic_element_id: "child".into(),
            direction: SemanticDependencyDirection::Both,
            max_depth: 1,
            include_descendants: true,
            include_inactive: false,
        },
    ] {
        let SemanticResult::ScopedGraph(expected) =
            execute(&local, SemanticOperation::ScopedRead(request.clone()))
        else {
            panic!("expected scoped graph");
        };
        assert_eq!(snapshot.scoped_graph(&request).unwrap(), expected);
    }
    assert!(
        snapshot
            .scoped_graph(&ScopedSemanticRead::Structure {
                scope: ProjectSnapshotScope::ProjectRoot("/foreign".into()),
                max_depth: 0,
                include_inactive: false
            })
            .is_err()
    );
}

#[test]
fn portable_snapshot_projection_preserves_duplicate_edge_aggregation() {
    let (_, mut snapshot) = publication();
    snapshot
        .relationships
        .push(snapshot.relationships[0].clone());
    let request = SemanticGraphProjectionRequest {
        project_root: "/project".into(),
        target_path: None,
        granularity: SemanticGraphGranularity::Property,
        recursive: true,
        include_external: false,
        include_first_neighbors: false,
    };
    let projected = snapshot.renderer_graph(&request).unwrap();
    assert_eq!(projected.edges.len(), 1);
    assert_eq!(projected.edges[0].weight, 2);
    assert!(
        snapshot
            .renderer_graph(&SemanticGraphProjectionRequest {
                project_root: "/foreign".into(),
                ..request
            })
            .is_err()
    );
}

#[test]
fn media_inheritance_rejects_nonobject_metadata_through_local_and_portable_paths() {
    use lumvise_db_core::SemanticArtifact;
    let local = LocalPersistence::in_memory().unwrap();
    let mut source = element("source", "image");
    source.content_fingerprint = Some("fp1:0000000000000001:hash".into());
    let mut target = element("target", "image");
    target.content_fingerprint = Some("fp1:0000000000000002:hash".into());
    execute(
        &local,
        SemanticOperation::SyncStructure {
            project_root: "/project".into(),
            elements: vec![source, target],
            relationships: Vec::new(),
        },
    );
    execute(
        &local,
        SemanticOperation::UpsertArtifact {
            artifact: SemanticArtifact {
                artifact_id: "note".into(),
                semantic_element_id: "source".into(),
                artifact_kind: "note".into(),
                title: "Note".into(),
                content_ref: None,
                content: None,
                searchable_text: None,
                content_size_bytes: None,
                dependencies: Vec::new(),
                metadata: serde_json::json!(["original"]),
            },
            media_type: "text/plain".into(),
        },
    );
    let operation = SemanticOperation::ArtifactsForElementWithInheritance {
        semantic_element_id: "target".into(),
    };
    assert!(
        local
            .execute(operation, &InvocationControl::sixty_seconds())
            .unwrap_err()
            .to_string()
            .contains("artifact metadata object")
    );
    let SemanticResult::ProjectSnapshot(snapshot) = execute(
        &local,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::ProjectRoot("/project".into()),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected snapshot");
    };
    assert!(
        snapshot
            .artifacts_with_inheritance("target")
            .unwrap_err()
            .to_string()
            .contains("artifact metadata object")
    );
    assert_eq!(
        snapshot.artifacts[0].metadata,
        serde_json::json!(["original"])
    );
}

#[test]
fn portable_search_and_selective_subgraph_match_public_local_reads() {
    let (local, snapshot) = publication();
    assert!(snapshot.search_element_candidates("", 20).is_err());
    for query in ["parent", "PARENT", "child name", "absent"] {
        let SemanticResult::Elements(expected) = execute(
            &local,
            SemanticOperation::SearchElementCandidates {
                project_root: Some("/project".into()),
                query: query.into(),
                limit: 20,
            },
        ) else {
            panic!("expected elements")
        };
        let mut actual = snapshot.search_element_candidates(query, 20).unwrap();
        actual.sort_by(|a, b| a.semantic_element_id.cmp(&b.semantic_element_id));
        let mut expected = expected;
        expected.sort_by(|a, b| a.semantic_element_id.cmp(&b.semantic_element_id));
        assert_eq!(actual, expected, "query {query}");
    }
    for root in ["parent", "child", "missing"] {
        let SemanticResult::SelectiveSubgraph(expected) = execute(
            &local,
            SemanticOperation::SelectiveSubgraph {
                project_root: "/project".into(),
                root_element_id: root.into(),
                artifact_namespace: None,
            },
        ) else {
            panic!("expected subtree")
        };
        assert_eq!(snapshot.selective_subgraph(root, None).unwrap(), expected);
    }
}

#[test]
fn complete_portable_subtree_handles_large_graphs_and_more_than_128_levels() {
    let (_, mut snapshot) = publication();
    snapshot.elements = (0..4_000)
        .map(|index| element(&index.to_string(), "folder"))
        .collect();
    snapshot.relationships = (1..4_000)
        .map(|index| SemanticRelationship {
            project_root: "/project".into(),
            source_element_id: (index - 1).to_string(),
            target_element_id: index.to_string(),
            relationship_kind: "contains".into(),
            label: "contains".into(),
            metadata: serde_json::json!({}),
        })
        .collect();
    let start = std::time::Instant::now();
    let selected = snapshot.selective_subgraph("0", None).unwrap().unwrap();
    eprintln!(
        "portable 4000-element complete traversal: {:?}",
        start.elapsed()
    );
    assert_eq!(selected.elements.len(), 4_000);
    assert_eq!(selected.relationships.len(), 3_999);
    assert!(snapshot.selective_subgraph("", None).is_err());
    assert!(
        snapshot
            .scoped_graph(&ScopedSemanticRead::Structure {
                scope: ProjectSnapshotScope::ProjectRoot("/project".into()),
                max_depth: 129,
                include_inactive: false
            })
            .is_err()
    );
}
