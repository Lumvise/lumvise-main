use crate::local::grafeo::graph_rows::insert_element_node;
use crate::{
    LocalPersistence, ProjectSnapshotScope, SemanticElement, SemanticOperation, SemanticPartition,
    SemanticPersistence, SemanticRelationship, SemanticResult,
};
use grafeo::Value;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

#[test]
fn duplicate_input_ids_are_rejected_without_publishing() {
    let persistence = LocalPersistence::in_memory().unwrap();
    for partition in [false, true] {
        let error = persistence
            .execute(
                sync_request(vec![element("a"), element("a")], vec![], partition),
                &InvocationControl::sixty_seconds(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("one incoming record per semantic element id")
        );
        assert_eq!(snapshot(&persistence).elements.len(), 0);
    }
}

#[test]
fn authoritative_sync_repairs_duplicates_and_preserves_native_attachments() {
    for partition in [false, true] {
        let persistence = duplicate_fixture();
        let incoming = vec![element("a"), element("b")];
        execute(
            &persistence,
            sync_request(incoming.clone(), vec![], partition),
        );
        let repaired = snapshot(&persistence);
        assert_eq!(repaired.artifacts.len(), 1);
        assert_eq!(
            repaired.artifacts[0].content.as_deref(),
            Some("important content")
        );
        assert_eq!(repaired.elements, incoming);
        assert_native_attachments(&persistence);
        execute(&persistence, sync_request(incoming, vec![], partition));
        assert_eq!(
            snapshot(&persistence).commit_version,
            repaired.commit_version
        );
        assert_native_attachments(&persistence);
        verify_export(&persistence, 2);
    }
}

#[test]
fn duplicate_repair_requires_an_authoritative_owner() {
    let persistence = duplicate_fixture();
    let error = persistence
        .execute(
            sync_request(vec![element("b")], vec![], false),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("authoritative incoming record"));
    assert_eq!(snapshot(&persistence).elements.len(), 4);
}

#[test]
fn staged_pages_reject_conflicting_ids_at_commit() {
    let persistence = LocalPersistence::in_memory().unwrap();
    execute(
        &persistence,
        SemanticOperation::BeginProjectSnapshot {
            snapshot_id: "duplicate-pages".into(),
            project_root: "/repo".into(),
            partition_paths: vec![],
            page_count: 2,
        },
    );
    for page_index in 0..2 {
        execute(
            &persistence,
            SemanticOperation::StageProjectSnapshot {
                snapshot_id: "duplicate-pages".into(),
                page_index,
                elements: vec![SemanticElement {
                    name: format!("owner-{page_index}"),
                    ..element("a")
                }],
                relationships: vec![],
            },
        );
    }
    let error = persistence
        .execute(
            SemanticOperation::CommitProjectSnapshot {
                snapshot_id: "duplicate-pages".into(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("identical records when a semantic id repeats")
    );
    assert!(snapshot(&persistence).elements.is_empty());
}

#[test]
fn staged_pages_coalesce_identical_source_records() {
    let persistence = LocalPersistence::in_memory().unwrap();
    execute(
        &persistence,
        SemanticOperation::BeginProjectSnapshot {
            snapshot_id: "repeated-source".into(),
            project_root: "/repo".into(),
            partition_paths: vec![],
            page_count: 2,
        },
    );
    for page_index in 0..2 {
        execute(
            &persistence,
            SemanticOperation::StageProjectSnapshot {
                snapshot_id: "repeated-source".into(),
                page_index,
                elements: vec![element("a")],
                relationships: vec![],
            },
        );
    }
    execute(
        &persistence,
        SemanticOperation::CommitProjectSnapshot {
            snapshot_id: "repeated-source".into(),
        },
    );
    assert_eq!(snapshot(&persistence).elements, vec![element("a")]);
    verify_export(&persistence, 1);
}

#[test]
fn duplicate_repair_keeps_updated_relationship_metadata() {
    let persistence = duplicate_fixture();
    let relationship = SemanticRelationship {
        project_root: "/repo".into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({"revision":2}),
    };
    execute(
        &persistence,
        sync_request(
            vec![element("a"), element("b")],
            vec![relationship.clone()],
            false,
        ),
    );
    assert_eq!(snapshot(&persistence).relationships, vec![relationship]);
    verify_export(&persistence, 2);
}

#[test]
fn large_duplicate_repair_retains_native_attachments() {
    let persistence = duplicate_fixture();
    let mut incoming = vec![element("a"), element("b")];
    incoming.extend((0..513).map(|index| element(&format!("extra-{index}"))));
    execute(&persistence, sync_request(incoming, vec![], false));
    assert_native_attachments(&persistence);
    verify_export(&persistence, 515);
}

fn duplicate_fixture() -> LocalPersistence {
    let persistence = LocalPersistence::in_memory().unwrap();
    persistence
        .core
        .graph
        .write(|graph| {
            let mut created = Vec::new();
            for id in ["a", "b", "a", "b"] {
                insert_element_node(graph, &element(id), 0)?;
                created.push(graph.semantic_node_id(id).unwrap());
            }
            let source = created[0];
            let target = created[1];
            let note = graph.create_node_with_props(
                &["SemanticArtifact"],
                [
                    ("artifact_id", Value::from("note")),
                    ("semantic_element_id", Value::from("a")),
                    ("artifact_kind", Value::from("note")),
                    ("title", Value::from("Keep this note")),
                    ("content", Value::from("important content")),
                ],
            )?;
            graph.create_edge_with_props(
                source,
                note,
                "semantic_artifact",
                [("artifact_id", Value::from("note"))],
            )?;
            graph.create_edge_with_props(
                note,
                target,
                "artifact_dependency",
                [("target_id", Value::from("b"))],
            )?;
            graph.create_edge_with_props(
                source,
                target,
                "custom_link",
                [("detail", Value::from("keep"))],
            )?;
            graph.create_edge_with_props(
                created[2],
                created[2],
                "custom_self",
                [("detail", Value::from("self"))],
            )?;
            for source in [source, created[2]] {
                graph.create_edge_with_props(
                    source,
                    target,
                    "calls",
                    [
                        ("project_root", Value::from("/repo")),
                        ("source_element_id", Value::from("a")),
                        ("target_element_id", Value::from("b")),
                        ("relationship_kind", Value::from("calls")),
                        ("label", Value::from("calls")),
                        ("metadata_json", Value::from("{\"revision\":1}")),
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
    persistence
}

fn assert_native_attachments(persistence: &LocalPersistence) {
    persistence.core.graph.read(|graph| {
        let nodes = graph.iter_nodes().collect::<Vec<_>>();
        assert_eq!(
            nodes
                .iter()
                .filter(|node| node.has_label("SemanticArtifact"))
                .count(),
            1
        );
        let edges = graph.iter_edges().collect::<Vec<_>>();
        assert_eq!(edges.len(), 4);
        for edge in edges {
            assert!(graph.get_node(edge.src).is_some());
            assert!(graph.get_node(edge.dst).is_some());
        }
    });
}

fn sync_request(
    elements: Vec<SemanticElement>,
    relationships: Vec<SemanticRelationship>,
    partition: bool,
) -> SemanticOperation {
    if partition {
        return SemanticOperation::SyncPartition {
            partition: SemanticPartition {
                project_root: "/repo".into(),
                replace_paths: vec!["src".into()],
            },
            elements,
            relationships,
        };
    }
    SemanticOperation::SyncStructure {
        project_root: "/repo".into(),
        elements,
        relationships,
    }
}

fn snapshot(persistence: &LocalPersistence) -> crate::SemanticProjectSnapshot {
    let SemanticResult::ProjectSnapshot(snapshot) = execute(
        persistence,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::ProjectRoot("/repo".into()),
            artifact_namespace: None,
        },
    ) else {
        panic!("expected project snapshot")
    };
    snapshot
}

fn execute(persistence: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    persistence
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn verify_export(persistence: &LocalPersistence, count: u64) {
    let directory = tempfile::tempdir().unwrap();
    let SemanticResult::PzSnapshot(snapshot) = execute(
        persistence,
        SemanticOperation::CreatePzSnapshot {
            project_root: "/repo".into(),
            output_path: directory
                .path()
                .join("repaired.pz")
                .to_string_lossy()
                .into_owned(),
        },
    ) else {
        panic!("expected PZ snapshot")
    };
    assert_eq!(snapshot.row_counts["elements.parquet"], count);
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: format!("src/{id}.rs"),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(2),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}
