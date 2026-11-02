mod common;
use common::*;
use lumvise_semantic_revisions::*;

struct MissingSourceFixture {
    report: SemanticRevisionIntegrityReport,
    request: SemanticRevisionRecoveryRequest,
}

impl MissingSourceFixture {
    fn new() -> Self {
        let mut project = ProjectFixture::new();
        project.initial();
        let mut headers = project
            .state
            .records
            .iter()
            .map(SemanticRevisionRecordHeader::from)
            .collect::<Vec<_>>();
        for header in &mut headers {
            header.element_scope.as_mut().unwrap().provider_instance_id =
                Some("source-parser".into());
        }
        let request = SemanticRevisionRecoveryRequest {
            operation_id: "acknowledge-body-loss".into(),
            project_root: ROOT.into(),
            project_id: PROJECT_ID.into(),
            expected_commit_version: 10,
            provider_instance_id: "source-parser".into(),
            missing_records: headers.iter().map(|header| header.record.clone()).collect(),
        };
        Self {
            report: SemanticRevisionIntegrityReport {
                publication_inspection: None,
                project_root: ROOT.into(),
                project_id: PROJECT_ID.into(),
                commit_version: 10,
                missing_records: headers,
            },
            request,
        }
    }

    fn plan(&self) -> SemanticRevisionResult<SemanticRevisionRecoveryPlan> {
        self.report.plan_recovery(&self.request, 20_000, 11)
    }
}

#[test]
fn recovery_changes_only_absence_metadata_and_records_exact_loss() {
    let fixture = MissingSourceFixture::new();
    let plan = fixture.plan().unwrap();
    assert_eq!(plan.expected_commit_version, 10);
    assert_eq!(plan.accepted.receipt.previous_commit_version, 10);
    assert_eq!(plan.accepted.receipt.commit_version, 11);
    assert_eq!(
        plan.accepted.receipt.unavailable_records,
        vec![
            SemanticRecordRef::element("a"),
            SemanticRecordRef::element("b")
        ]
    );
    for (before, after) in fixture.report.missing_records.iter().zip(&plan.headers) {
        let mut preserved = after.clone();
        preserved.unavailable = None;
        assert_eq!(&preserved, before);
        let marker = after.unavailable.clone().unwrap();
        assert_eq!(marker.recovery_operation_id, "acknowledge-body-loss");
        assert_eq!(marker.unavailable_at_ms, 20_000);
        assert_eq!(marker.canonical_lifecycle.as_deref(), Some("active"));
        assert!(
            !after
                .clone()
                .restore(SemanticRevisionBody::Unavailable(marker))
                .unwrap()
                .body
                .is_available()
        );
    }
}

#[test]
fn recovery_rejects_other_projects_and_stale_commits() {
    let mut fixture = MissingSourceFixture::new();
    fixture.request.project_id = "another-project".into();
    assert!(
        fixture
            .plan()
            .unwrap_err()
            .to_string()
            .contains("another-project")
    );
    fixture.request.project_id = PROJECT_ID.into();
    fixture.request.expected_commit_version = 9;
    assert!(fixture.plan().is_err());
    fixture.request.expected_commit_version = 10;
    assert!(
        fixture
            .report
            .plan_recovery(&fixture.request, 20_000, 10)
            .is_err()
    );
    fixture.request.project_root = "/another-root".into();
    assert!(fixture.plan().is_err());
}

#[test]
fn recovery_requires_the_entire_missing_set_without_duplicates() {
    let mut fixture = MissingSourceFixture::new();
    fixture.request.missing_records.pop();
    assert!(fixture.plan().is_err());
    fixture
        .request
        .missing_records
        .push(SemanticRecordRef::element("unexpected"));
    assert!(
        fixture
            .plan()
            .unwrap_err()
            .to_string()
            .contains("unexpected")
    );
    fixture.request.missing_records = vec![
        SemanticRecordRef::element("a"),
        SemanticRecordRef::element("a"),
    ];
    assert!(fixture.plan().is_err());
    fixture.request.missing_records.clear();
    fixture.report.missing_records.clear();
    assert!(fixture.plan().is_err());
}

#[test]
fn recovery_cannot_acknowledge_other_providers_or_already_absent_records() {
    let mut fixture = MissingSourceFixture::new();
    fixture.request.provider_instance_id = "other-parser".into();
    assert!(
        fixture
            .plan()
            .unwrap_err()
            .to_string()
            .contains("other-parser")
    );
    fixture.request.provider_instance_id = "source-parser".into();
    fixture.report.missing_records[0].pruned_at_ms = Some(15_000);
    assert!(fixture.plan().is_err());
    fixture.report.missing_records[0].pruned_at_ms = None;
    fixture.report.missing_records[0].unavailable = Some(SemanticRevisionUnavailable {
        recovery_operation_id: "earlier-recovery".into(),
        unavailable_at_ms: 15_000,
        canonical_lifecycle: Some("active".into()),
    });
    assert!(fixture.plan().is_err());
}

#[test]
fn accepted_recovery_retries_reordered_intent_but_rejects_changed_scope() {
    let mut fixture = MissingSourceFixture::new();
    let accepted = fixture.plan().unwrap().accepted;
    fixture.request.missing_records.reverse();
    assert_eq!(
        accepted.replay(&fixture.request).unwrap().commit_version,
        11
    );
    fixture.request.expected_commit_version = 11;
    assert!(accepted.replay(&fixture.request).is_err());
    fixture.request.expected_commit_version = 10;
    fixture.request.provider_instance_id = "other-parser".into();
    assert!(accepted.replay(&fixture.request).is_err());
}

#[test]
fn recovery_cannot_accept_an_empty_exact_record_identity() {
    let mut fixture = MissingSourceFixture::new();
    fixture.report.missing_records[0].record.record_id.clear();
    fixture.request.missing_records[0].record_id.clear();
    assert!(fixture.plan().is_err());
}
