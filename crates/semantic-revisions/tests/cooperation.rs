//! Public policy conformance used by every native and gateway adapter.
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_semantic_revisions::*;
use serde_json::json;

mod common;
use common::*;

#[test]
fn exact_old_body_survives_updates_and_old_parser_ids_find_the_accepted_successor() {
    let mut project = ProjectFixture::new();
    project.initial();
    let original = project
        .state
        .resolve(&SemanticRecordRef::element("a"))
        .unwrap()
        .body;
    let request = project.edit(
        "second",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let receipt = project.publish(&request);
    let second = accepted_id(&receipt, "a");
    assert_ne!(second, "a");
    assert_eq!(
        project
            .state
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .body,
        original
    );
    let request = project.edit(
        "unchanged",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    assert_eq!(accepted_id(&project.publish(&request), "a"), second);
    let request = project.edit(
        "revert",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    let reverted = accepted_id(&project.publish(&request), "a");
    assert_ne!(reverted, "a");
    assert_ne!(reverted, second);
}

#[test]
fn disjoint_instances_preserve_both_edits_and_overlapping_stale_writers_conflict() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut first = project.edit("first", vec![element("a", "a.rs", "A2")]);
    path_edit(&mut first, "a.rs");
    let mut second = project.edit("second", vec![element("b", "b.rs", "B2")]);
    path_edit(&mut second, "b.rs");
    let mut stale = first.clone();
    stale.operation_id = "stale".into();
    project.publish(&first);
    project.publish(&second);
    assert_eq!(
        project
            .selected()
            .elements
            .iter()
            .map(|element| element.metadata["body"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        ["A2", "B2"].into_iter().collect()
    );
    let conflict = project.plan(&stale);
    assert!(matches!(
        conflict.outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
    assert!(conflict.records.is_empty());
}

#[test]
fn retry_after_a_lost_acknowledgement_returns_the_identical_receipt() {
    let mut project = ProjectFixture::new();
    project.initial();
    let request = project.edit("lost-ack", vec![element("a", "a.rs", "A2")]);
    let receipt = project.publish(&request);
    let before = project.state.clone();
    assert_eq!(project.publish(&request), receipt);
    assert_eq!(project.state, before);
    let mut changed = request;
    changed.structure.as_mut().unwrap().elements[0].name = "different".into();
    let error = project
        .state
        .plan_publication(&changed, &timestamp(5000), project.state.commit_version + 1)
        .unwrap_err();
    assert!(error.to_string().contains("identical content"));
}

#[test]
fn deleting_an_edge_changes_the_selection_even_when_both_endpoint_ids_stay() {
    let mut project = ProjectFixture::new();
    let mut request = project.edit(
        "initial",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    let edge = SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    };
    request.structure.as_mut().unwrap().relationships = vec![edge.clone(), edge];
    project.publish(&request);
    assert_eq!(project.selected().connections.len(), 2);
    let request = project.edit(
        "delete-edges",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    project.publish(&request);
    assert!(project.selected().connections.is_empty());
    assert_eq!(
        project
            .selected()
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
}

#[test]
fn accepted_source_updates_inherit_artifact_attachment_without_rewriting_its_body() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit(
        "artifact",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    let original = artifact("note", "a", "A note");
    let expected = SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: original.clone(),
        media_type: "text/plain".into(),
    }
    .stored_artifact(PROJECT_ID)
    .unwrap();
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: original.clone(),
        media_type: "text/plain".into(),
    });
    project.publish(&request);
    let request = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let receipt = project.publish(&request);
    let newer = accepted_id(&receipt, "a");
    assert_eq!(
        project
            .state
            .resolve(&SemanticRecordRef::artifact("note"))
            .unwrap()
            .body,
        SemanticRevisionBody::Artifact(expected)
    );
    assert!(
        project
            .state
            .associations
            .iter()
            .any(|association| association.artifact_id == "note"
                && association.semantic_element_id == newer
                && association.inherited)
    );
    let receipt = project.collect();
    assert!(
        receipt
            .pruned_records
            .contains(&SemanticRecordRef::element("a"))
    );
    assert_eq!(
        project
            .state
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .body,
        SemanticRevisionBody::Pruned
    );
    assert_eq!(
        project
            .state
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .successors[0]
            .newer
            .record_id,
        newer
    );
}

#[test]
fn required_evidence_keeps_the_exact_older_body_after_contextual_attachment_advances() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit(
        "artifact",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    let mut note = artifact("note", "a", "Required evidence");
    note.dependencies.push(ArtifactDependency {
        target: ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "a".into(),
        },
    });
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: note,
        media_type: "text/plain".into(),
    });
    project.publish(&request);
    let request = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    project.publish(&request);
    assert!(
        !project
            .collect()
            .pruned_records
            .contains(&SemanticRecordRef::element("a"))
    );
    assert!(matches!(
        project
            .state
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .body,
        SemanticRevisionBody::Element(_)
    ));
}

#[test]
fn unrooted_artifact_cycles_are_collectable_and_shared_blobs_survive_a_retained_revision() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit(
        "cycle",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    for (id, other) in [("x", "y"), ("y", "x")] {
        let mut note = artifact(id, "a", id);
        note.content = None;
        note.content_ref = Some("blob:shared".into());
        note.dependencies.push(ArtifactDependency {
            target: ArtifactDependencyTarget::Artifact {
                edit_version: None,
                artifact_id: other.into(),
            },
        });
        request.artifacts.push(SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact: note,
            media_type: "text/plain".into(),
        });
    }
    let mut retained = artifact("retained", "a", "Retained");
    retained.content = None;
    retained.content_ref = Some("blob:shared".into());
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: retained,
        media_type: "text/plain".into(),
    });
    project.publish(&request);
    let mut request = project.edit(
        "remove-cycle",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    request.removed_artifact_ids = vec!["x".into(), "y".into()];
    project.publish(&request);
    let receipt = project.collect();
    assert_eq!(
        receipt.pruned_records,
        vec![
            SemanticRecordRef::artifact("x"),
            SemanticRecordRef::artifact("y")
        ]
    );
    assert!(receipt.removed_blob_refs.is_empty());
}

#[test]
fn a_new_acknowledged_pin_invalidates_an_old_collection_plan_and_protects_its_body() {
    let mut project = ProjectFixture::new();
    project.initial();
    let request = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    project.publish(&request);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let stale = project
        .state
        .plan_retention("stale", &policy, 20_000)
        .unwrap();
    assert!(stale.records.contains(&SemanticRecordRef::element("a")));
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "pin".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "public".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::element("a")],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(&pin, &timestamp(3000), project.state.commit_version + 1)
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    assert!(
        project
            .state
            .apply_retention(&stale, &policy, 20_000, project.state.commit_version + 1)
            .is_err()
    );
    assert!(
        !project
            .collect()
            .pruned_records
            .contains(&SemanticRecordRef::element("a"))
    );
}

#[test]
fn adoption_preserves_existing_record_ids_payloads_and_connections() {
    let elements = vec![element("a", "a.rs", "A1")];
    let note = artifact("note", "a", "Existing note");
    let state = SemanticRevisionState::adopt(
        ROOT,
        PROJECT_ID,
        "working",
        "git:main",
        elements.clone(),
        vec![note.clone()],
        vec![],
        9,
        1000,
    )
    .unwrap();
    let selected = state.snapshot("working", "time").unwrap().unwrap();
    assert_eq!(selected.elements, elements);
    assert_eq!(selected.artifacts, vec![note]);
    assert_eq!(state.commit_version, 9);
}

#[test]
fn parent_revision_changes_remap_children_and_unchanged_reindex_keeps_containment() {
    let mut project = ProjectFixture::new();
    let parent = element("parent", "a.rs", "parent 1");
    let mut child = element("child", "a.rs", "child");
    child.parent_element_id = Some("parent".into());
    let request = project.edit("parents", vec![parent.clone(), child.clone()]);
    project.publish(&request);
    assert_eq!(project.selected().connections.len(), 1);
    let request = project.edit("unchanged-parents", vec![parent, child.clone()]);
    project.publish(&request);
    assert_eq!(project.selected().connections.len(), 1);
    let request = project.edit(
        "changed-parent",
        vec![element("parent", "a.rs", "parent 2"), child],
    );
    let receipt = project.publish(&request);
    let parent_id = accepted_id(&receipt, "parent");
    let child_id = accepted_id(&receipt, "child");
    assert_ne!(parent_id, "parent");
    assert_ne!(child_id, "child");
    let selected = project.selected();
    assert_eq!(
        selected
            .elements
            .iter()
            .find(|element| element.semantic_element_id == child_id)
            .unwrap()
            .parent_element_id
            .as_ref(),
        Some(&parent_id)
    );
    assert_eq!(
        selected.connections[0].relationship.source_element_id,
        parent_id
    );
    assert_eq!(
        selected.connections[0].relationship.target_element_id,
        child_id
    );
}

#[test]
fn working_selection_accepts_deletions_while_pins_and_other_contexts_keep_exact_ids() {
    let mut project = ProjectFixture::new();
    project.initial();
    for (id, context, mode) in [
        ("pinned", "git:main", SemanticSelectionMode::Pinned),
        (
            "other-branch",
            "git:other",
            SemanticSelectionMode::FollowAccepted,
        ),
    ] {
        let update = SemanticRevisionSelectionUpdate {
            artifact_edits: Default::default(),
            operation_id: format!("select-{id}"),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            selection_id: id.into(),
            source_context: context.into(),
            expected_generation: 0,
            mode,
            records: project.selected().expectation.records,
            connection_ids: vec![],
            source_anchor: None,
        };
        let plan = project
            .state
            .plan_selection(&update, &timestamp(2000), project.state.commit_version + 1)
            .unwrap();
        project.state.apply_publication(plan).unwrap();
    }
    let request = project.edit("delete-a", vec![element("b", "b.rs", "B2")]);
    project.publish(&request);
    let current = project.selected();
    assert_eq!(current.elements.len(), 1);
    assert_eq!(current.elements[0].metadata["body"], "B2");
    assert_eq!(
        project
            .state
            .snapshot("pinned", "time")
            .unwrap()
            .unwrap()
            .elements
            .len(),
        2
    );
    let other = project
        .state
        .snapshot("other-branch", "time")
        .unwrap()
        .unwrap();
    assert!(
        other
            .elements
            .iter()
            .any(|element| element.semantic_element_id == "a")
    );
    assert!(
        other
            .elements
            .iter()
            .any(|element| element.semantic_element_id == "b")
    );
}

#[test]
fn a_delayed_sweep_uses_its_fenced_cutoff_and_stale_pruned_scopes_still_conflict() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut stale = project.edit(
        "stale-a",
        vec![element("new-a", "a.rs", "stale resurrection")],
    );
    path_edit(&mut stale, "a.rs");
    let request = project.edit("remove-a", vec![element("b", "b.rs", "B1")]);
    project.publish(&request);
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = project
        .state
        .plan_retention("delayed", &policy, 20_000)
        .unwrap();
    project
        .state
        .apply_retention(&plan, &policy, 21_000, project.state.commit_version + 1)
        .unwrap();
    assert!(matches!(
        project.plan(&stale).outcome,
        SemanticRevisionPublicationOutcome::Conflict(_)
    ));
}

#[test]
fn matching_timestamp_alone_reuses_a_body_but_changed_evidence_gets_a_new_revision() {
    let mut project = ProjectFixture::new();
    let mut source = element("a", "a.rs", "body");
    source.match_evidence = Some(SemanticMatchEvidence {
        match_confidence: 90,
        simhash_distance: Some(1),
        matched_at: "first".into(),
        precaution: None,
    });
    let request = project.edit("initial", vec![source.clone()]);
    project.publish(&request);
    source.match_evidence.as_mut().unwrap().matched_at = "later".into();
    let request = project.edit("timestamp", vec![source.clone()]);
    assert_eq!(accepted_id(&project.publish(&request), "a"), "a");
    source.match_evidence.as_mut().unwrap().match_confidence = 80;
    let request = project.edit("evidence-change", vec![source]);
    assert_ne!(accepted_id(&project.publish(&request), "a"), "a");
}

#[test]
fn an_idempotent_retry_accepts_reordered_metadata_keys_across_native_and_wasm_builds() {
    let mut project = ProjectFixture::new();
    let mut source = element("a", "a.rs", "body");
    source.metadata = serde_json::from_str(r#"{"z":2,"a":{"y":4,"b":3}}"#).unwrap();
    let request = project.edit("same-operation", vec![source]);
    let receipt = project.publish(&request);
    let mut reordered = request;
    reordered.structure.as_mut().unwrap().elements[0].metadata =
        serde_json::from_str(r#"{"a":{"b":3,"y":4},"z":2}"#).unwrap();
    assert_eq!(project.publish(&reordered), receipt);
}

#[test]
fn artifact_edits_preserve_the_source_anchor_and_unanchored_source_changes_mark_it_dirty() {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit("initial", vec![element("a", "a.rs", "A1")]);
    let anchor = SemanticSourceAnchor {
        revision: "commit-1".into(),
        canonical_remote: Some("https://example.invalid/project.git".into()),
        dirty: false,
    };
    initial.source_anchor = Some(anchor.clone());
    project.publish(&initial);
    let mut note_edit = project.edit("artifact", vec![]);
    note_edit.structure = None;
    note_edit.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "Note"),
        media_type: "text/plain".into(),
    });
    project.publish(&note_edit);
    assert_eq!(project.selected().selection.source_anchor, Some(anchor));
    let source_edit = project.edit("watcher-edit", vec![element("a", "a.rs", "A2")]);
    project.publish(&source_edit);
    assert!(project.selected().selection.source_anchor.unwrap().dirty);
}

#[test]
fn accepted_selection_replays_after_release_and_collection_without_restoring_the_pin() {
    let mut project = ProjectFixture::new();
    project.initial();
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "old-pin".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "pin".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::element("a")],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(&pin, &timestamp(3000), project.state.commit_version + 1)
        .unwrap();
    let accepted = project.state.apply_publication(plan).unwrap();
    let edit = project.edit("replace-a", vec![element("a", "a.rs", "A2")]);
    project.publish(&edit);
    let release = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "release-pin".into(),
        expected_generation: 1,
        records: vec![],
        ..pin.clone()
    };
    let plan = project
        .state
        .plan_selection(&release, &timestamp(4000), project.state.commit_version + 1)
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    assert!(
        project
            .collect()
            .pruned_records
            .contains(&SemanticRecordRef::element("a"))
    );
    let before = project.state.clone();
    let replay = project
        .state
        .plan_selection(&pin, &timestamp(30_000), project.state.commit_version + 1)
        .unwrap();
    assert_eq!(project.state.apply_publication(replay).unwrap(), accepted);
    assert_eq!(project.state, before);
}

#[test]
fn duplicate_followers_are_rejected_and_connection_changes_mark_the_source_anchor_dirty() {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit(
        "initial",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    initial.source_anchor = Some(SemanticSourceAnchor {
        revision: "commit-1".into(),
        canonical_remote: None,
        dirty: false,
    });
    project.publish(&initial);
    let head = project.selected();
    let follower = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "follow".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "follower".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::FollowAccepted,
        records: head.expectation.records,
        connection_ids: head.expectation.connection_ids,
        source_anchor: head.selection.source_anchor,
    };
    assert!(
        project
            .state
            .plan_selection(
                &follower,
                &timestamp(3000),
                project.state.commit_version + 1
            )
            .is_err()
    );
    let mut edit = project.edit(
        "advance",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    edit.source_anchor = Some(SemanticSourceAnchor {
        revision: "commit-2".into(),
        canonical_remote: None,
        dirty: false,
    });
    project.publish(&edit);
    assert_eq!(
        project.selected().selection.source_anchor,
        edit.source_anchor
    );
    let mut edge_edit = project.edit("edge-change", project.selected().elements);
    let ids = &edge_edit.structure.as_ref().unwrap().elements;
    let connection = SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: ids[0].semantic_element_id.clone(),
        target_element_id: ids[1].semantic_element_id.clone(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    };
    edge_edit.structure.as_mut().unwrap().relationships = vec![connection];
    project.publish(&edge_edit);
    assert!(project.selected().selection.source_anchor.unwrap().dirty);
}

#[test]
fn membership_deltas_reconstruct_old_anchors_and_edges_after_payload_collection() {
    let mut project = ProjectFixture::new();
    let mut initial = project.edit(
        "initial",
        vec![element("a", "a.rs", "A1"), element("b", "b.rs", "B1")],
    );
    let anchor = SemanticSourceAnchor {
        revision: "commit-1".into(),
        canonical_remote: None,
        dirty: false,
    };
    initial.source_anchor = Some(anchor.clone());
    initial.structure.as_mut().unwrap().relationships = vec![SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }];
    project.publish(&initial);
    let old = project.state.selection("working").unwrap().clone();
    let edge_removal = project.edit("remove-edge", project.selected().elements);
    project.publish(&edge_removal);
    let delta = &project.state.selection_history[1];
    assert!(delta.added_records.is_empty());
    assert!(delta.removed_records.is_empty());
    assert_eq!(delta.removed_connection_ids, old.connection_ids);
    let changed = project.edit("new-source", vec![element("a", "a.rs", "A2")]);
    project.publish(&changed);
    project.collect();
    assert!(matches!(
        project
            .state
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .body,
        SemanticRevisionBody::Pruned
    ));
    assert_eq!(
        project.state.selection_at_generation("working", 1).unwrap(),
        Some(old.clone())
    );
    assert_eq!(
        project
            .state
            .selection_for_source_anchor("working", &anchor)
            .unwrap(),
        Some(old)
    );
    assert_eq!(project.state.connections.len(), 1);
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "restore-pruned".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "historical".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::element("a")],
        connection_ids: vec![],
        source_anchor: Some(anchor),
    };
    assert!(
        project
            .state
            .plan_selection(&pin, &timestamp(30_000), project.state.commit_version + 1)
            .is_err()
    );
}

#[test]
fn broken_membership_history_never_silently_selects_different_content() {
    let mut project = ProjectFixture::new();
    project.initial();
    let head = project.state.selection("working").unwrap();
    let mut delta =
        SemanticRevisionSelectionDelta::between(None, head, project.state.commit_version);
    delta.previous_generation = 9;
    assert!(delta.apply_to(None).is_err());
    delta.previous_generation = 0;
    delta
        .removed_records
        .push(SemanticRecordRef::element("missing"));
    assert!(delta.apply_to(None).is_err());
    delta.removed_records.clear();
    delta.added_records.push(delta.added_records[0].clone());
    assert!(delta.apply_to(None).is_err());
}

#[test]
fn scoped_artifact_basis_changes_only_its_memberships_and_requires_exact_coverage() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut note_edit = project.edit("note", vec![]);
    note_edit.structure = None;
    note_edit.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "Scoped note"),
        media_type: "text/plain".into(),
    });
    let mut facts = project.state.clone();
    facts
        .records
        .retain(|stored| stored.record.record_id != "b");
    for head in &mut facts.selections {
        head.records.retain(|record| record.record_id != "b");
    }
    let mut basis = SemanticRevisionPublicationBasis {
        structure_coverage: SemanticRevisionStructureCoverage::None,
        artifact_ids: requested_artifact_ids(&note_edit),
        facts,
    };
    let plan = basis
        .plan_publication(
            &note_edit,
            &timestamp(3000),
            project.state.commit_version + 1,
        )
        .unwrap();
    let complete = project.plan(&note_edit);
    assert_eq!(plan.selection_deltas, complete.selection_deltas);
    project.state.apply_publication(plan).unwrap();
    assert!(
        project
            .state
            .selection("working")
            .unwrap()
            .records
            .contains(&SemanticRecordRef::element("b"))
    );
    assert_eq!(
        project
            .state
            .records
            .iter()
            .find(|stored| stored.record.record_id == "b")
            .unwrap()
            .unselected_at_ms,
        None
    );
    basis.artifact_ids.clear();
    assert!(
        basis
            .plan_publication(
                &note_edit,
                &timestamp(3000),
                project.state.commit_version + 1
            )
            .is_err()
    );
}
