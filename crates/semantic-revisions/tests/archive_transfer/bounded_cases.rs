//! Real large rows exercise the exact first-five-response loader, including partial history proofs.
use super::*;
use lumvise_semantic_revisions::SemanticRelationship;

fn archive_of(project: common::ProjectFixture) -> String {
    let commit = project.state.commit_version;
    json!({"display_name":null,"source_revision":null,"selection_id":"working","revision_state":project.state,
        "project_id":PROJECT_ID,"snapshot_id":"bounded-groups-v1","canonical_remote":null,
        "snapshot":{"project_root":ROOT,"commit_version":commit,"published_at":"","elements":[],"artifacts":[],"relationships":[]},
        "artifact_blobs":[],"artifact_text_vectors":[],"element_name_vectors":[]}).to_string()
}

#[test]
fn public_connection_dependencies_still_require_explicit_public_membership() {
    for hide_parent in [false, true] {
        let mut project = common::ProjectFixture::new();
        project.state.project_id = PROJECT_ID.into();
        let parent = common::element("public-parent", "parent.rs", "parent");
        let mut child = common::element("public-child", "child.rs", "child");
        child.parent_element_id = Some("public-parent".into());
        project.publish(&project.edit("public-connection", vec![parent, child]));
        let head = project.state.selection("working").unwrap();
        let request = lumvise_semantic_revisions::SemanticRevisionSelectionUpdate {
            operation_id: "pin-public-fixture".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            selection_id: "public".into(),
            source_context: "published".into(),
            expected_generation: 0,
            artifact_edits: Default::default(),
            mode: lumvise_semantic_revisions::SemanticSelectionMode::Pinned,
            records: head
                .records
                .iter()
                .filter(|record| !hide_parent || record.record_id != "public-parent")
                .cloned()
                .collect(),
            connection_ids: head.connection_ids.clone(),
            source_anchor: None,
        };
        let plan = project
            .state
            .plan_selection(&request, &common::timestamp(2000), 2)
            .unwrap();
        project.state.apply_publication(plan).unwrap();
        let archive = archive_of(project);
        let pages = lumvise_semantic_revisions::encode_archive_transfer_pages(&archive).unwrap();
        let mut adapter = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
        if hide_parent {
            assert!(
                adapter
                    .run()
                    .unwrap_err()
                    .contains("explicitly published dependency")
            );
        } else {
            adapter.run().unwrap();
        }
    }
}

#[test]
fn two_sub_megabyte_bodies_and_selected_connection_converge_with_first_five_facts() {
    let mut project = common::ProjectFixture::new();
    project.state.project_id = PROJECT_ID.into();
    let parent = common::element("large-parent", "parent.rs", &"P".repeat(700 * 1024));
    let mut child = common::element("large-child", "child.rs", &"C".repeat(700 * 1024));
    child.parent_element_id = Some("large-parent".into());
    let mut publication = project.edit("large-groups", vec![parent, child]);
    publication.structure.as_mut().unwrap().relationships = vec![SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "large-parent".into(),
        target_element_id: "large-child".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({"payload":"E".repeat(700 * 1024)}),
    }];
    project.publish(&publication);
    let archive = archive_of(project);
    let expected = commit_archive(&empty_state(), &json!([]), &archive, MERGE_NOW_MS);
    let pages = lumvise_semantic_revisions::encode_archive_transfer_pages(&archive).unwrap();
    let mut adapter = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
    let accepted = adapter.run().unwrap();
    let restored = fixture::storage(
        json!({"action":"state","project_root":"/online","project_id":PROJECT_ID,
        "commit_version":accepted["commit_version"],"rows":adapter.applied_rows()}),
    );
    let state: SemanticRevisionState =
        serde_json::from_str(restored["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(state, expected.0);
    // Parent hints also publish their structural containment edge through the existing owner.
    assert_eq!(state.selection("working").unwrap().connection_ids.len(), 2);
    assert!(
        adapter.peak_fact_bytes > 1400 * 1024,
        "the proof must cross the aggregate one-MiB target"
    );
    assert!(adapter.peak_fact_bytes <= 5 * (adapter.largest_row + 1024 * 1024));
}

#[test]
fn oversized_history_and_scratch_members_commit_validated_prefixes() {
    let mut project = common::ProjectFixture::new();
    project.state.project_id = PROJECT_ID.into();
    let elements = (0..16)
        .map(|index| {
            common::element(
                &format!("large-id-{index}-{}", "x".repeat(70 * 1024)),
                &format!("large/{index}.rs"),
                "history proof",
            )
        })
        .collect();
    project.publish(&project.edit("oversized-history", elements));
    let archive = archive_of(project);
    let pages = lumvise_semantic_revisions::encode_archive_transfer_pages(&archive).unwrap();
    let mut adapter = incremental::ArchiveCatalog::new(&pages, &json!([]), 0);
    let accepted = adapter.run().unwrap();
    let restored = fixture::storage(
        json!({"action":"state","project_root":"/online","project_id":PROJECT_ID,
        "commit_version":accepted["commit_version"],"rows":adapter.applied_rows()}),
    );
    let state: SemanticRevisionState =
        serde_json::from_str(restored["value_json"].as_str().unwrap()).unwrap();
    assert_eq!(state.selection("working").unwrap().records.len(), 16);
    assert_eq!(
        state.selection_at_generation("working", 1).unwrap(),
        state.selection("working").cloned()
    );
    assert!(adapter.peak_fact_bytes <= 5 * (adapter.largest_row + 1024 * 1024));
}
