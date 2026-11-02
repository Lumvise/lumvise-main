mod common;
use common::*;
use lumvise_semantic_revisions::*;

struct AcknowledgedSourceLoss {
    project: ProjectFixture,
    intact: SemanticRevisionState,
}

impl AcknowledgedSourceLoss {
    fn new() -> Self {
        let mut project = ProjectFixture::new();
        let mut a = element("a", "a.rs", "old bytes");
        a.metadata["provider_instance_id"] = "source-parser".into();
        let request = project.edit("initial", vec![a, element("b", "b.rs", "intact bytes")]);
        project.publish(&request);
        let intact = project.state.clone();
        let header = SemanticRevisionRecordHeader::from(&project.state.records[0]);
        let report = SemanticRevisionIntegrityReport {
            publication_inspection: None,
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            commit_version: project.state.commit_version,
            missing_records: vec![header],
        };
        let request = SemanticRevisionRecoveryRequest {
            operation_id: "acknowledge-loss".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            expected_commit_version: project.state.commit_version,
            provider_instance_id: "source-parser".into(),
            missing_records: vec![SemanticRecordRef::element("a")],
        };
        let plan = report
            .plan_recovery(&request, 2000, project.state.commit_version + 1)
            .unwrap();
        let header = plan.headers[0].clone();
        project.state.records[0] = header
            .clone()
            .restore(header.payload_absence().unwrap())
            .unwrap();
        project.state.commit_version = plan.accepted.receipt.commit_version;
        project.state.recovery_operations.push(plan.accepted);
        Self { project, intact }
    }

    fn replace_a(&mut self) {
        let mut a = element("a", "a.rs", "freshly scanned bytes");
        a.metadata["provider_instance_id"] = "source-parser".into();
        let mut request = self.project.edit("rescan-a", vec![a]);
        path_edit(&mut request, "a.rs");
        self.project.publish(&request);
    }
}

#[test]
fn acknowledgement_preserves_membership_and_history_but_reads_fail_explicitly() {
    let fixture = AcknowledgedSourceLoss::new();
    let state = &fixture.project.state;
    assert_eq!(state.selections, fixture.intact.selections);
    assert_eq!(state.selection_history, fixture.intact.selection_history);
    assert_eq!(state.records[1], fixture.intact.records[1]);
    let error = state.snapshot("working", "").unwrap_err().to_string();
    assert!(error.contains("a"), "{error}");
    assert!(error.contains("acknowledge-loss"), "{error}");
    assert!(matches!(
        state
            .resolve(&SemanticRecordRef::element("a"))
            .unwrap()
            .body,
        SemanticRevisionBody::Unavailable(_)
    ));
}

#[test]
fn rescan_allocates_a_successor_and_replaces_the_unavailable_scoped_member() {
    let mut fixture = AcknowledgedSourceLoss::new();
    fixture.replace_a();
    let snapshot = fixture.project.selected();
    assert_eq!(snapshot.elements.len(), 2);
    assert!(
        snapshot
            .elements
            .iter()
            .any(|element| element.semantic_element_id == "b")
    );
    let replacement = snapshot
        .elements
        .iter()
        .find(|element| element.path == "a.rs")
        .unwrap();
    assert_ne!(replacement.semantic_element_id, "a");
    assert_eq!(replacement.metadata["body"], "freshly scanned bytes");
    let old = fixture
        .project
        .state
        .resolve(&SemanticRecordRef::element("a"))
        .unwrap();
    assert!(matches!(old.body, SemanticRevisionBody::Unavailable(_)));
    assert_eq!(old.successors.len(), 1);
    assert_eq!(
        old.successors[0].newer.record_id,
        replacement.semantic_element_id
    );
    assert!(
        fixture
            .project
            .state
            .selection("working")
            .unwrap()
            .records
            .iter()
            .all(|record| record.record_id != "a")
    );
    assert_eq!(
        fixture
            .project
            .state
            .records
            .iter()
            .find(|record| record.record.record_id == "b")
            .unwrap(),
        &fixture.intact.records[1]
    );
}

#[test]
fn acknowledged_loss_round_trips_in_archive_metadata_without_substituting_bodies() {
    let fixture = AcknowledgedSourceLoss::new();
    let state = fixture.project.state;
    let projection = state.archive_projection("working").unwrap().unwrap();
    assert_eq!(
        projection
            .elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b"]
    );
    assert!(projection.artifacts.is_empty());
    let restored = SemanticRevisionState::from_metadata_rows(
        ROOT,
        state.commit_version,
        state.metadata_rows(),
        vec![element("b", "b.rs", "intact bytes")],
        vec![],
    )
    .unwrap();
    assert_eq!(restored, state);
    assert!(restored.snapshot("working", "").is_err());
    let unaudited = state
        .metadata_rows()
        .into_iter()
        .filter(|row| !matches!(row, SemanticRevisionMetadata::Recovery(_)))
        .collect();
    assert!(
        SemanticRevisionState::from_metadata_rows(
            ROOT,
            state.commit_version,
            unaudited,
            vec![element("b", "b.rs", "intact bytes")],
            vec![]
        )
        .is_err()
    );
}

#[test]
fn a_new_pin_or_dependency_cannot_select_an_unavailable_body() {
    let fixture = AcknowledgedSourceLoss::new();
    let state = &fixture.project.state;
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "new-pin".into(),
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
    assert!(
        state
            .plan_selection(&pin, &timestamp(3000), state.commit_version + 1)
            .is_err()
    );
    let mut child = element("child", "child.rs", "child bytes");
    child.parent_element_id = Some("a".into());
    let mut request = fixture.project.edit("new-child", vec![child]);
    path_edit(&mut request, "child.rs");
    assert!(
        state
            .plan_publication(&request, &timestamp(3000), state.commit_version + 1)
            .is_err()
    );
}

#[test]
fn retention_keeps_loss_audit_distinct_and_archive_merge_cannot_resurrect_it() {
    let mut fixture = AcknowledgedSourceLoss::new();
    assert!(
        fixture
            .project
            .state
            .merge_archive(&fixture.intact, 5000)
            .is_err()
    );
    fixture.replace_a();
    let policy = SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = fixture
        .project
        .state
        .plan_retention("collect", &policy, 20_000)
        .unwrap();
    assert!(!plan.records.contains(&SemanticRecordRef::element("a")));
    fixture
        .project
        .state
        .apply_retention(
            &plan,
            &policy,
            20_000,
            fixture.project.state.commit_version + 1,
        )
        .unwrap();
    fixture.project.state.validate_archive_state().unwrap();
    assert_eq!(fixture.project.state.recovery_operations.len(), 1);
}
