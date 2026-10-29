mod common;
use common::*;
use lumvise_semantic_revisions::*;

fn archive_bodies(state: &SemanticRevisionState) -> (Vec<SemanticElement>, Vec<SemanticArtifact>) {
    let mut elements = vec![];
    let mut artifacts = vec![];
    for stored in &state.records {
        match &stored.body {
            SemanticRevisionBody::Element(element) => elements.push(element.clone()),
            SemanticRevisionBody::Artifact(artifact) => artifacts.push(artifact.clone()),
            SemanticRevisionBody::Pruned => {}
        }
    }
    (elements, artifacts)
}

#[test]
fn unchanged_inline_artifact_reuses_its_revision_after_project_root_rebinding() {
    let mut project = ProjectFixture::new();
    project.initial();
    let write = SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "Unchanged body"),
        media_type: "text/plain".into(),
    };
    let mut request = project.edit("note", vec![]);
    request.structure = None;
    request.artifacts.push(write.clone());
    project.publish(&request);
    let (elements, artifacts) = archive_bodies(&project.state);
    let mut restored = SemanticRevisionState::from_metadata_rows(
        "/relocated",
        project.state.commit_version,
        project.state.metadata_rows(),
        elements,
        artifacts,
    )
    .unwrap();
    let mut request = request;
    request.project_root = "/relocated".into();
    request.operation_id = "unchanged-after-relocation".into();
    request.expected = restored
        .selection("working")
        .unwrap()
        .expectation(&restored.records);
    let plan = restored
        .plan_publication(&request, &timestamp(4000), restored.commit_version + 1)
        .unwrap();
    let SemanticRevisionPublicationOutcome::Accepted(receipt) =
        restored.apply_publication(plan).unwrap()
    else {
        panic!("expected unchanged publication");
    };
    assert_eq!(accepted_id(&receipt, "note"), "note");
    assert!(receipt.revision_links.is_empty());
}

#[test]
fn publication_requires_a_durable_project_identity_before_allocating_any_revisions() {
    let mut project = ProjectFixture::new();
    project.state.project_id.clear();
    let request = project.edit("initial", vec![element("a", "a.rs", "body")]);
    assert!(
        project
            .state
            .plan_publication(&request, &timestamp(1000), 1)
            .unwrap_err()
            .to_string()
            .contains("non-empty durable project identity")
    );
}

#[test]
fn metadata_round_trip_preserves_history_and_replays_acknowledgements_after_root_rebinding() {
    let mut project = ProjectFixture::new();
    project.initial();
    let edit = project.edit(
        "lost-ack",
        vec![element("a", "a.rs", "new"), element("b", "b.rs", "B1")],
    );
    let receipt = project.publish(&edit);
    project.collect();
    let encoded = serde_json::to_vec(&project.state.metadata_rows()).unwrap();
    let rows = serde_json::from_slice(&encoded).unwrap();
    let (elements, artifacts) = archive_bodies(&project.state);
    let mut restored = SemanticRevisionState::from_metadata_rows(
        "/restored",
        project.state.commit_version,
        rows,
        elements,
        artifacts,
    )
    .unwrap();
    assert_eq!(
        restored
            .selection("working")
            .unwrap()
            .expectation(&restored.records),
        project
            .state
            .selection("working")
            .unwrap()
            .expectation(&project.state.records)
    );
    assert_eq!(
        restored
            .selection_at_generation("working", 1)
            .unwrap()
            .unwrap()
            .records,
        vec![
            SemanticRecordRef::element("a"),
            SemanticRecordRef::element("b")
        ]
    );
    assert!(matches!(
        restored
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .body,
        SemanticRevisionBody::Pruned
    ));
    let mut retry = edit;
    retry.project_root = "/restored".into();
    for element in &mut retry.structure.as_mut().unwrap().elements {
        element.project_root = "/restored".into();
    }
    let plan = restored
        .plan_publication(&retry, &timestamp(30_000), restored.commit_version + 1)
        .unwrap();
    let mut expected = receipt;
    expected.selection.project_root = "/restored".into();
    for link in &mut expected.revision_links {
        link.project_root = "/restored".into();
    }
    let before = restored.clone();
    assert_eq!(
        restored.apply_publication(plan).unwrap(),
        SemanticRevisionPublicationOutcome::Accepted(expected)
    );
    assert_eq!(restored, before);
}

#[test]
fn metadata_rows_have_unique_identities_and_never_duplicate_canonical_body_content() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit("note", vec![]);
    request.structure = None;
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "private-body-only-once"),
        media_type: "text/plain".into(),
    });
    project.publish(&request);
    let rows = project.state.metadata_rows();
    let keys = rows
        .iter()
        .map(|row| (row.kind(), row.identity().unwrap()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), rows.len());
    assert!(
        !String::from_utf8(serde_json::to_vec(&rows).unwrap())
            .unwrap()
            .contains("private-body-only-once")
    );
    for row in rows {
        if let SemanticRevisionMetadata::Membership(member) = &row {
            assert_eq!(
                row.membership_selection_id(),
                Some(member.selection_id.as_str())
            );
            assert!(member.record().is_some() || member.connection_id().is_some());
        }
    }
}

#[test]
fn missing_or_duplicate_bodies_and_incomplete_history_are_rejected() {
    let mut project = ProjectFixture::new();
    project.initial();
    let rows = project.state.metadata_rows();
    let (elements, artifacts) = archive_bodies(&project.state);
    assert!(
        SemanticRevisionState::from_metadata_rows(ROOT, 1, rows.clone(), vec![], vec![]).is_err()
    );
    let mut duplicate = elements.clone();
    duplicate.push(elements[0].clone());
    assert!(
        SemanticRevisionState::from_metadata_rows(
            ROOT,
            1,
            rows.clone(),
            duplicate,
            artifacts.clone()
        )
        .is_err()
    );
    let incomplete = rows
        .iter()
        .filter(|row| !matches!(row, SemanticRevisionMetadata::History(_)))
        .cloned()
        .collect();
    assert!(
        SemanticRevisionState::from_metadata_rows(
            ROOT,
            1,
            incomplete,
            elements.clone(),
            artifacts.clone()
        )
        .is_err()
    );
    let mut duplicate_rows = rows.clone();
    duplicate_rows.push(rows[0].clone());
    assert!(
        SemanticRevisionState::from_metadata_rows(ROOT, 1, duplicate_rows, elements, artifacts)
            .is_err()
    );
}

#[test]
fn archived_routing_is_relative_and_attachment_ancestry_survives_body_collection() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut note_request = project.edit("note", vec![]);
    note_request.structure = None;
    note_request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "old"),
        media_type: "text/plain".into(),
    });
    project.publish(&note_request);
    let mut changed = note_request;
    changed.operation_id = "changed-note".into();
    changed.expected = project.selected().expectation;
    changed.artifacts[0].artifact.content = Some("new".into());
    let receipt = project.publish(&changed);
    project.collect();
    let owners = project.state.retained_artifact_owner_ids();
    assert!(owners.contains("note"));
    assert!(owners.contains(&accepted_id(&receipt, "note")));
    let rows = project.state.archive_metadata_rows();
    assert!(
        rows.iter()
            .filter_map(|row| match row {
                SemanticRevisionMetadata::Selection(head) => Some(head.project_root.as_str()),
                _ => None,
            })
            .all(str::is_empty)
    );
    let (elements, artifacts) = archive_bodies(&project.state);
    let restored = SemanticRevisionState::from_metadata_rows(
        "/rebound",
        project.state.commit_version,
        rows,
        elements,
        artifacts,
    )
    .unwrap();
    assert_eq!(restored.retained_artifact_owner_ids(), owners);
    assert!(
        restored
            .accepted_operations
            .iter()
            .all(|operation| operation.request_root == ROOT)
    );
}

#[test]
fn selected_associations_follow_the_exact_head_without_changing_the_artifact_owner() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut request = project.edit("note", vec![]);
    request.structure = None;
    request.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: artifact("note", "a", "unchanged note"),
        media_type: "text/plain".into(),
    });
    project.publish(&request);
    let edit = project.edit(
        "source-edit",
        vec![element("a", "a.rs", "A2"), element("b", "b.rs", "B1")],
    );
    let receipt = project.publish(&edit);
    let associations = project.state.selected_associations("working");
    assert_eq!(associations.len(), 1);
    assert_eq!(associations[0].artifact_id, "note");
    assert_eq!(
        associations[0].semantic_element_id,
        accepted_id(&receipt, "a")
    );
    assert_eq!(project.selected().artifacts[0].semantic_element_id, "a");
    assert!(project.state.selected_associations("missing").is_empty());
}

#[test]
fn archive_validation_rejects_missing_exact_dependencies_and_future_acknowledgements() {
    use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
    let mut project = ProjectFixture::new();
    project.initial();
    let mut body = artifact("note", "a", "evidence");
    body.dependencies.push(ArtifactDependency {
        target: ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: "missing".into(),
        },
    });
    assert_eq!(
        body.revision_dependencies(),
        vec![SemanticRecordRef::element("missing")]
    );
    assert_eq!(
        SemanticRevisionBody::Artifact(body.clone()).intrinsic_dependencies(),
        body.revision_dependencies()
    );
    let mut rows = project.state.metadata_rows();
    let body_row = SemanticStoredRevision {
        artifact_state: Some(SemanticArtifactRevisionState {
            edit_version: 1,
            frozen: true,
        }),
        record: SemanticRecordRef::artifact("note"),
        element_scope: None,
        body: SemanticRevisionBody::Artifact(body.clone()),
        created_at_ms: 1000,
        unselected_at_ms: Some(2000),
        pruned_at_ms: None,
    };
    rows.push(SemanticRevisionMetadata::Record((&body_row).into()));
    let (elements, mut artifacts) = archive_bodies(&project.state);
    artifacts.push(body);
    assert!(
        SemanticRevisionState::from_metadata_rows(
            ROOT,
            project.state.commit_version,
            rows,
            elements.clone(),
            artifacts
        )
        .is_err()
    );
    assert!(
        SemanticRevisionState::from_metadata_rows(
            ROOT,
            0,
            project.state.metadata_rows(),
            elements,
            vec![]
        )
        .is_err()
    );
}

#[test]
fn unchanged_connections_keep_their_ids_and_clean_anchor_after_root_rebinding() {
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
    initial
        .structure
        .as_mut()
        .unwrap()
        .relationships
        .push(SemanticRelationship {
            project_root: ROOT.into(),
            source_element_id: "a".into(),
            target_element_id: "b".into(),
            relationship_kind: "calls".into(),
            label: "calls".into(),
            metadata: serde_json::json!({}),
        });
    project.publish(&initial);
    let imported_id = "externally-assigned-connection".to_string();
    project.state.connections[0].connection_id = imported_id.clone();
    project.state.selections[0].connection_ids[0] = imported_id.clone();
    project.state.selection_history[0].added_connection_ids[0] = imported_id;
    let (mut elements, artifacts) = archive_bodies(&project.state);
    for body in &mut elements {
        body.project_root = "/rebound".into();
    }
    let restored = SemanticRevisionState::from_metadata_rows(
        "/rebound",
        project.state.commit_version,
        project.state.metadata_rows(),
        elements.clone(),
        artifacts,
    )
    .unwrap();
    let mut update = initial;
    update.project_root = "/rebound".into();
    update.operation_id = "unchanged-reindex".into();
    update.source_anchor = None;
    update.expected = restored
        .selection("working")
        .unwrap()
        .expectation(&restored.records);
    update.structure.as_mut().unwrap().elements = elements;
    update.structure.as_mut().unwrap().relationships[0].project_root = "/rebound".into();
    let plan = restored
        .plan_publication(&update, &timestamp(9000), restored.commit_version + 1)
        .unwrap();
    let after = plan
        .selections
        .iter()
        .find(|head| head.selection.selection_id == "working")
        .unwrap();
    assert_eq!(after.connection_ids, update.expected.connection_ids);
    assert_eq!(
        after.selection.source_anchor,
        restored
            .selection("working")
            .unwrap()
            .selection
            .source_anchor
    );
}
