//! Archive bodies retain available exact historical dependencies without successor substitution.
mod common;
use common::*;
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_semantic_revisions::*;
use std::collections::BTreeSet;

struct HistoricalEvidenceFixture {
    project: ProjectFixture,
    old_a: SemanticRecordRef,
    old_b: SemanticRecordRef,
    old_c: SemanticRecordRef,
    successor_a: SemanticRecordRef,
    successor_b: SemanticRecordRef,
    successor_c: SemanticRecordRef,
}

fn source_element(id: &str, body: &str) -> SemanticElement {
    let mut source = element(id, &format!("{id}.rs"), body);
    source.metadata["provider_instance_id"] = "source-parser".into();
    source
}

fn old_b_element() -> SemanticElement {
    let mut source = source_element("b", "B1");
    source.revision_dependencies = vec![SemanticRecordRef::element("c")];
    source
}

fn evidence_artifact() -> SemanticArtifact {
    let mut evidence = artifact("historical-evidence", "a", "Historical evidence");
    evidence.dependencies = ["a", "b"]
        .map(|semantic_element_id| ArtifactDependency {
            target: ArtifactDependencyTarget::SemanticElement {
                semantic_element_id: semantic_element_id.into(),
            },
        })
        .into();
    evidence
}

fn publish_initial_evidence(project: &mut ProjectFixture) {
    let mut initial = project.edit(
        "initial-evidence",
        vec![
            source_element("a", "A1"),
            old_b_element(),
            source_element("c", "C1"),
        ],
    );
    initial.artifacts = vec![SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: evidence_artifact(),
        media_type: "text/plain".into(),
    }];
    project.publish(&initial);
}

fn advance_sources(project: &mut ProjectFixture) -> SemanticRevisionPublicationReceipt {
    let update = project.edit(
        "advance-sources",
        vec![
            source_element("a", "A2"),
            source_element("b", "B2"),
            source_element("c", "C2"),
        ],
    );
    project.publish(&update)
}

fn historical_evidence_fixture() -> HistoricalEvidenceFixture {
    let mut project = ProjectFixture::new();
    publish_initial_evidence(&mut project);
    let receipt = advance_sources(&mut project);
    HistoricalEvidenceFixture {
        project,
        old_a: SemanticRecordRef::element("a"),
        old_b: SemanticRecordRef::element("b"),
        old_c: SemanticRecordRef::element("c"),
        successor_a: SemanticRecordRef::element(accepted_id(&receipt, "a")),
        successor_b: SemanticRecordRef::element(accepted_id(&receipt, "b")),
        successor_c: SemanticRecordRef::element(accepted_id(&receipt, "c")),
    }
}

fn recovery_report(
    project: &ProjectFixture,
    missing: &SemanticRecordRef,
) -> SemanticRevisionIntegrityReport {
    let stored = project
        .state
        .records
        .iter()
        .find(|record| &record.record == missing)
        .unwrap();
    SemanticRevisionIntegrityReport {
        publication_inspection: None,
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        commit_version: project.state.commit_version,
        missing_records: vec![SemanticRevisionRecordHeader::from(stored)],
    }
}

fn recovery_request(
    project: &ProjectFixture,
    missing: &SemanticRecordRef,
) -> SemanticRevisionRecoveryRequest {
    SemanticRevisionRecoveryRequest {
        operation_id: "acknowledge-historical-a-loss".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        expected_commit_version: project.state.commit_version,
        provider_instance_id: "source-parser".into(),
        missing_records: vec![missing.clone()],
    }
}

fn acknowledge_unavailable_source(project: &mut ProjectFixture, missing: &SemanticRecordRef) {
    let report = recovery_report(project, missing);
    let request = recovery_request(project, missing);
    let plan = report
        .plan_recovery(&request, project.now, project.state.commit_version + 1)
        .unwrap();
    let unavailable = plan.headers[0].clone();
    let index = project
        .state
        .records
        .iter()
        .position(|record| &record.record == missing)
        .unwrap();
    project.state.records[index] = unavailable
        .clone()
        .restore(unavailable.payload_absence().unwrap())
        .unwrap();
    project.state.commit_version = plan.accepted.receipt.commit_version;
    project.state.recovery_operations.push(plan.accepted);
}

fn selected_projection_records(
    projection: &SemanticRevisionArchiveProjection,
) -> BTreeSet<SemanticRecordRef> {
    projection
        .elements
        .iter()
        .map(|element| SemanticRecordRef::element(&element.semantic_element_id))
        .chain(
            projection
                .artifacts
                .iter()
                .map(|artifact| SemanticRecordRef::artifact(&artifact.artifact_id)),
        )
        .collect()
}

#[test]
fn archive_projection_includes_literal_available_historical_evidence() {
    let fixture = historical_evidence_fixture();
    let projection = fixture
        .project
        .state
        .archive_projection("working")
        .unwrap()
        .unwrap();

    assert_eq!(
        projection
            .required_records
            .iter()
            .map(|record| record.record.clone())
            .collect::<Vec<_>>(),
        vec![
            SemanticRecordRef::element("a"),
            SemanticRecordRef::element("b"),
            SemanticRecordRef::element("c"),
        ]
    );
    assert_eq!(
        projection.required_records[0].body,
        SemanticRevisionBody::Element(source_element("a", "A1"))
    );
    assert_eq!(
        projection.required_records[1].body,
        SemanticRevisionBody::Element(old_b_element())
    );
    assert_eq!(
        projection.required_records[2].body,
        SemanticRevisionBody::Element(source_element("c", "C1"))
    );
    assert_ne!(fixture.successor_a, fixture.old_a);
    assert_ne!(fixture.successor_b, fixture.old_b);
    assert_ne!(fixture.successor_c, fixture.old_c);
}

#[test]
fn archive_projection_skips_only_acknowledged_loss_without_successor_substitution() {
    let mut fixture = historical_evidence_fixture();
    let selected_before = fixture
        .project
        .state
        .selection("working")
        .unwrap()
        .records
        .clone();
    acknowledge_unavailable_source(&mut fixture.project, &fixture.old_a);

    let projection = fixture
        .project
        .state
        .archive_projection("working")
        .unwrap()
        .unwrap();
    assert_eq!(
        projection
            .required_records
            .iter()
            .map(|record| record.record.clone())
            .collect::<Vec<_>>(),
        vec![
            SemanticRecordRef::element("b"),
            SemanticRecordRef::element("c")
        ]
    );
    assert_eq!(
        projection.required_records[0].body,
        SemanticRevisionBody::Element(old_b_element())
    );
    assert_eq!(
        projection.required_records[1].body,
        SemanticRevisionBody::Element(source_element("c", "C1"))
    );
    assert_eq!(
        projection.artifacts[0].dependencies,
        evidence_artifact().dependencies
    );
    assert!(
        projection
            .elements
            .iter()
            .any(|element| { element.semantic_element_id == fixture.successor_a.record_id })
    );
    assert!(!projection.required_records.iter().any(|record| {
        record.record == fixture.old_a
            || record.record == fixture.successor_a
            || record.record == fixture.successor_b
            || record.record == fixture.successor_c
    }));
    assert_eq!(
        selected_projection_records(&projection),
        selected_before.iter().cloned().collect()
    );
    assert_eq!(
        fixture.project.state.selection("working").unwrap().records,
        selected_before
    );
    assert!(matches!(
        fixture.project.state.resolve(&fixture.old_a).unwrap().body,
        SemanticRevisionBody::Unavailable(_)
    ));
    assert!(
        fixture
            .project
            .state
            .snapshot("working", "2026-10-24T00:00:00Z")
            .unwrap_err()
            .to_string()
            .contains("acknowledge-historical-a-loss")
    );
}
