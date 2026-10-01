use super::*;
use crate::local::grafeo::graph_row_projection::i64_property;
use crate::local::grafeo::graph_store::GraphStore;
use serde_json::json;

#[test]
fn semantic_relationships_are_stored_as_native_edges() {
    let store = graph_store();
    store
        .write(|graph| {
            insert_element_node(graph, &element("source"), 1)?;
            insert_element_node(graph, &element("target"), 1)?;
            insert_relationship_node(graph, &relationship("source", "target"))
        })
        .unwrap();

    store.read(|database| {
        assert_eq!(database.iter_edges().count(), 1);
        assert_eq!(
            database
                .iter_nodes()
                .filter(|node| node.has_label("SemanticRelationship"))
                .count(),
            0
        );
    });
}

#[test]
fn semantic_element_replacement_keeps_one_resolvable_node() {
    let store = graph_store();
    store
        .write(|graph| insert_element_node(graph, &element("stable"), 1))
        .unwrap();
    store
        .write(|graph| upsert_element_node(graph, &element("stable"), 2))
        .unwrap();

    store.read(|database| {
        assert!(semantic_element_node_id(database, "stable").is_some());
        assert_eq!(semantic_elements_for_project(database, "/repo").len(), 1);
    });
}

#[test]
fn touching_relationships_use_one_edge_property_batch() {
    let store = graph_store();
    store
        .write(|graph| {
            insert_element_node(graph, &element("root"), 1)?;
            for index in 0..32 {
                let id = format!("child-{index}");
                insert_element_node(graph, &element(&id), 1)?;
                insert_relationship_node(graph, &relationship("root", &id))?;
            }
            Ok(())
        })
        .unwrap();
    store.read(|graph| {
        let ids = (0..32)
            .map(|index| format!("child-{index}"))
            .chain(std::iter::once("root".into()))
            .collect();
        reset_selective_batch_counters();
        assert_eq!(
            semantic_relationships_touching_elements(graph, &ids).len(),
            32
        );
        assert_eq!(selective_batch_counters().1, 1);
    });
}

#[test]
fn semantic_element_by_id_ignores_label_conflict_on_same_property() {
    let store = graph_store();
    store
        .write(|graph| {
            graph.create_node_with_props(
                &["SemanticArtifact"],
                [
                    (SEMANTIC_ELEMENT_ID_PROPERTY, GrafeoValue::from("shared")),
                    (PATH_PROPERTY, GrafeoValue::from("shared.txt")),
                    ("artifact_id", GrafeoValue::from("artifact-shared")),
                ],
            )?;
            insert_element_node(graph, &element("shared"), 1)
        })
        .unwrap();

    store.read(|database| {
        let element = semantic_element_by_id(database, "shared").unwrap();
        assert_eq!(element.semantic_element_id, "shared");
        assert_eq!(element.path, "src/shared.rs");
    });
}

#[test]
fn semantic_artifact_alias_mapping_is_graph_properties() {
    let store = graph_store();
    store
        .write(|graph| upsert_artifact_node(graph, &alias_artifact()))
        .unwrap();

    store.read(|database| {
        let artifact = database
            .iter_nodes()
            .find(|node| node.has_label("SemanticArtifact"))
            .unwrap();
        assert_eq!(
            string_property(&artifact, "storage_alias_target_uri").as_deref(),
            Some("file:///repo/.lumvise/storage/doc.md")
        );
    });
}

#[test]
fn semantic_artifacts_are_read_from_native_element_edges() {
    let store = graph_store();
    store
        .write(|graph| {
            insert_element_node(graph, &alias_element(), 1)?;
            upsert_artifact_node(graph, &alias_artifact())?;
            Ok(())
        })
        .unwrap();

    store.read(|database| {
        let artifacts = semantic_artifacts_for_element(database, "file:doc.csv");

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_id, "artifact");
        assert_eq!(database.iter_edges().count(), 1);
    });
}

#[test]
fn semantic_artifact_upsert_replaces_native_element_edge() {
    let store = graph_store();
    store
        .write(|graph| {
            insert_element_node(graph, &alias_element(), 1)?;
            upsert_artifact_node(graph, &alias_artifact())
        })
        .unwrap();
    store
        .write(|graph| upsert_artifact_node(graph, &alias_artifact()))
        .unwrap();

    store.read(|database| {
        let artifacts = semantic_artifacts_for_element(database, "file:doc.csv");

        assert_eq!(artifacts.len(), 1);
        assert_eq!(database.iter_edges().count(), 1);
    });
}

#[test]
fn semantic_element_alias_mapping_is_graph_properties() {
    let store = graph_store();
    store
        .write(|graph| insert_element_node(graph, &alias_element(), 1))
        .unwrap();

    store.read(|database| {
        let element = database
            .iter_nodes()
            .find(|node| node.has_label("SemanticElement"))
            .unwrap();
        assert_eq!(
            string_property(&element, "storage_alias_target_uri").as_deref(),
            Some("file:///repo/.lumvise/storage/doc.md")
        );
    });
}

#[test]
fn complete_project_rows_use_fixed_selective_column_batches() {
    let store = graph_store();
    store
        .write(|graph| {
            insert_element_node(graph, &alias_element(), 1)?;
            insert_element_node(graph, &element("target"), 1)?;
            let mut inactive = element("inactive");
            inactive.lifecycle = "inactive".into();
            insert_element_node(graph, &inactive, 1)?;
            insert_relationship_node(graph, &relationship("file:doc.csv", "target"))?;
            upsert_artifact_node(graph, &alias_artifact())?;
            let mut inactive_artifact = alias_artifact();
            inactive_artifact.artifact_id = "inactive-artifact".into();
            inactive_artifact.semantic_element_id = "inactive".into();
            upsert_artifact_node(graph, &inactive_artifact)
        })
        .unwrap();
    reset_selective_batch_counters();
    store.read(|database| {
        let rows = complete_project_rows(database, "/repo");
        assert_eq!(rows.elements.len(), 3);
        assert_eq!(rows.relationships.len(), 1);
        assert_eq!(rows.artifacts.len(), 2);
        assert_eq!(selective_batch_counters(), (2, 1));
        reset_selective_batch_counters();
        let elements = semantic_elements_for_project_including_inactive(database, "/repo");
        assert_eq!(elements.len(), 3);
        assert_eq!(selective_batch_counters(), (1, 0));
        reset_selective_batch_counters();
        assert_eq!(
            project_artifacts_selective(database, "/repo", None).len(),
            2
        );
        assert_eq!(selective_batch_counters(), (2, 0));
    });
}

fn graph_store() -> GraphStore {
    GraphStore::new(GrafeoDB::new_in_memory())
}

fn element(id: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".to_string(),
        semantic_element_id: id.to_string(),
        semantic_source_id: "source".to_string(),
        path: format!("src/{id}.rs"),
        element_kind: "file".to_string(),
        name: id.to_string(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".to_string(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn relationship(source_id: &str, target_id: &str) -> SemanticRelationship {
    SemanticRelationship {
        project_root: "/repo".to_string(),
        source_element_id: source_id.to_string(),
        target_element_id: target_id.to_string(),
        relationship_kind: "contains".to_string(),
        label: "contains".to_string(),
        metadata: json!({}),
    }
}

fn alias_artifact() -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "artifact".to_string(),
        semantic_element_id: "file:doc.csv".to_string(),
        artifact_kind: "derived_markdown".to_string(),
        title: "Markdown view".to_string(),
        content_ref: Some("alias://anytomd/doc".to_string()),
        content: None,
        searchable_text: Some("table".to_string()),
        content_size_bytes: Some(12),
        dependencies: vec![],
        metadata: json!({
            "storage_alias": {
                "target_uri": "file:///repo/.lumvise/storage/doc.md"
            }
        }),
    }
}

fn alias_element() -> SemanticElement {
    SemanticElement {
        metadata: json!({
            "storage_alias": {
                "target_uri": "file:///repo/.lumvise/storage/doc.md"
            }
        }),
        ..element("file:doc.csv")
    }
}

#[test]
fn identity_backfill_makes_pre_upgrade_elements_candidate_visible_immediately_and_is_idempotent() {
    let store = graph_store();
    store
        .write(|graph| {
            insert_pre_upgrade_element(
                graph,
                &SemanticElement {
                    element_kind: "function".into(),
                    name: "Shared".into(),
                    path: "src/shared.rs".into(),
                    content_fingerprint: Some("fp1:0000000000000001:same-hash".into()),
                    ..element("old-source")
                },
                1,
            )?;
            insert_pre_upgrade_element(
                graph,
                &SemanticElement {
                    element_kind: "function".into(),
                    name: "shared".into(),
                    path: "other/shared.rs".into(),
                    content_fingerprint: Some("fp1:0000000000000001:same-hash".into()),
                    ..element("old-target")
                },
                1,
            )
        })
        .unwrap();

    // Before any backfill/index registration runs, neither derived-key bucket
    // can see these pre-upgrade nodes (their identity_kind_name/
    // identity_kind_file_name properties were never written).
    store.read(|database| {
        assert!(
            candidate_elements_by_identity_keys(
                database,
                &HashSet::new(),
                &HashSet::from(["function\u{1}shared".to_string()]),
                &HashSet::new(),
            )
            .is_empty()
        );
    });

    let snapshot = store.snapshot();
    configure_semantic_indexes(&snapshot).expect("first-open backfill");

    let (revision_after_first, active_after_first) = store.read(|database| {
        let kind_name = candidate_elements_by_identity_keys(
            database,
            &HashSet::new(),
            &HashSet::from(["function\u{1}shared".to_string()]),
            &HashSet::new(),
        );
        assert_eq!(
            kind_name.len(),
            2,
            "both pre-upgrade elements become visible via the kind+name bucket \
             immediately after the backfill runs - no temporary recall gap"
        );
        let kind_file_name = candidate_elements_by_identity_keys(
            database,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::from(["function\u{1}shared.rs".to_string()]),
        );
        assert_eq!(kind_file_name.len(), 2, "and via the kind+file-name bucket");
        let fingerprint = candidate_elements_by_identity_keys(
            database,
            &HashSet::from(["fp1:0000000000000001:same-hash".to_string()]),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(
            fingerprint.len(),
            2,
            "and via the raw content_fingerprint bucket"
        );
        assert!(
            candidate_elements_by_identity_keys(
                database,
                &HashSet::new(),
                &HashSet::from(["function\u{1}unrelated".to_string()]),
                &HashSet::new(),
            )
            .is_empty(),
            "a non-matching key excludes both elements"
        );
        let source_node = database
            .find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from("old-source"),
            )
            .into_iter()
            .find_map(|node_id| database.get_node(node_id))
            .expect("backfilled source node");
        (
            i64_property(&source_node, "last_changed_revision"),
            bool_property(&source_node, "active"),
        )
    });
    assert_eq!(
        revision_after_first,
        Some(1),
        "backfill must never bump last_changed_revision - no semantic commit is published"
    );
    assert!(
        active_after_first,
        "backfill must never touch the active/deleted_at change-state properties"
    );
    assert_eq!(
        store
            .snapshot()
            .iter_nodes()
            .filter(|node| node.has_label(IDENTITY_BACKFILL_MARKER_LABEL))
            .count(),
        1
    );

    // A second open/second configure_semantic_indexes call is idempotent: the
    // marker already exists, so it must not touch element properties again
    // (verified by an unchanged revision) or duplicate the marker node.
    configure_semantic_indexes(&store.snapshot()).expect("idempotent second-open backfill");
    store.read(|database| {
        let kind_name = candidate_elements_by_identity_keys(
            database,
            &HashSet::new(),
            &HashSet::from(["function\u{1}shared".to_string()]),
            &HashSet::new(),
        );
        assert_eq!(kind_name.len(), 2);
        let source_node = database
            .find_nodes_by_property(
                SEMANTIC_ELEMENT_ID_PROPERTY,
                &GrafeoValue::from("old-source"),
            )
            .into_iter()
            .find_map(|node_id| database.get_node(node_id))
            .expect("backfilled source node");
        assert_eq!(
            i64_property(&source_node, "last_changed_revision"),
            Some(1),
            "the idempotent second run must not touch last_changed_revision either"
        );
    });
    assert_eq!(
        store
            .snapshot()
            .iter_nodes()
            .filter(|node| node.has_label(IDENTITY_BACKFILL_MARKER_LABEL))
            .count(),
        1,
        "a second configure_semantic_indexes call must not create a second marker node"
    );
}

fn insert_pre_upgrade_element(
    database: &GraphTransaction<'_>,
    element: &SemanticElement,
    commit_version: i64,
) -> Result<()> {
    let properties = vec![
        (
            PROJECT_ROOT_PROPERTY,
            GrafeoValue::from(element.project_root.clone()),
        ),
        (
            SEMANTIC_ELEMENT_ID_PROPERTY,
            GrafeoValue::from(element.semantic_element_id.clone()),
        ),
        (
            "semantic_source_id",
            GrafeoValue::from(element.semantic_source_id.clone()),
        ),
        (
            PARENT_ELEMENT_ID_PROPERTY,
            GrafeoValue::from(element.parent_element_id.clone().unwrap_or_default()),
        ),
        (PATH_PROPERTY, GrafeoValue::from(element.path.clone())),
        (
            "element_kind",
            GrafeoValue::from(element.element_kind.clone()),
        ),
        ("name", GrafeoValue::from(element.name.clone())),
        (
            CONTENT_FINGERPRINT_PROPERTY,
            GrafeoValue::from(element.content_fingerprint.clone().unwrap_or_default()),
        ),
        (
            "start_line",
            GrafeoValue::from(element.start_line.unwrap_or(-1)),
        ),
        (
            "end_line",
            GrafeoValue::from(element.end_line.unwrap_or(-1)),
        ),
        ("lifecycle", GrafeoValue::from(element.lifecycle.clone())),
        ("match_confidence", GrafeoValue::from(-1i64)),
        ("simhash_distance", GrafeoValue::from(-1i64)),
        ("matched_at", GrafeoValue::from("")),
        ("precaution", GrafeoValue::from("")),
        (
            "metadata_json",
            GrafeoValue::from(serde_json::to_string(&element.metadata)?),
        ),
        (
            LAST_CHANGED_REVISION_PROPERTY,
            GrafeoValue::from(commit_version),
        ),
        (ACTIVE_PROPERTY, GrafeoValue::from(true)),
        (DELETED_AT_PROPERTY, GrafeoValue::from(-1i64)),
        // Deliberately omits identity_kind_name/identity_kind_file_name,
        // simulating a SemanticElement node written before this migration
        // existed.
    ];
    database.create_semantic_node(&element.semantic_element_id, properties)?;
    Ok(())
}
