//! Configurable limits count immutable record ancestry, while selections and exact evidence stay protected.
mod common;
use common::*;
use lumvise_contracts::{ArtifactDependency, ArtifactDependencyTarget};
use lumvise_semantic_revisions::*;

fn source_history(versions: u32) -> (ProjectFixture, Vec<SemanticRecordRef>) {
    let mut project = ProjectFixture::new();
    let mut revisions = vec![];
    for version in 0..versions {
        let request = project.edit(
            &format!("source-{version}"),
            vec![
                element("a", "a.rs", &format!("A{version}")),
                element("b", "b.rs", "B0"),
            ],
        );
        let receipt = project.publish(&request);
        revisions.push(SemanticRecordRef::element(accepted_id(&receipt, "a")));
    }
    (project, revisions)
}

#[test]
fn default_is_thirty_days_and_modes_round_trip_without_accepting_zero_limits() {
    assert_eq!(
        SemanticRevisionRetentionPolicy::default(),
        SemanticRevisionRetentionPolicy::Age {
            history_age_seconds: 30 * 24 * 60 * 60
        }
    );
    let (project, _) = source_history(2);
    for policy in [
        SemanticRevisionRetentionPolicy::default(),
        SemanticRevisionRetentionPolicy::RevisionCount { revisions: 10 },
    ] {
        let encoded = serde_json::to_vec(&policy).unwrap();
        assert_eq!(
            serde_json::from_slice::<SemanticRevisionRetentionPolicy>(&encoded).unwrap(),
            policy
        );
    }
    for policy in [
        SemanticRevisionRetentionPolicy::Age {
            history_age_seconds: 0,
        },
        SemanticRevisionRetentionPolicy::RevisionCount { revisions: 0 },
    ] {
        assert!(
            project
                .state
                .plan_retention("invalid", &policy, 100_000)
                .is_err()
        );
    }
}

#[test]
fn count_keeps_ten_revisions_including_current_while_age_has_its_own_limit() {
    let (project, revisions) = source_history(12);
    let count = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 10 };
    let plan = project
        .state
        .plan_retention("count", &count, 100_000)
        .unwrap();
    assert_eq!(
        plan.records
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        revisions[..2].iter().cloned().collect()
    );
    let young = project
        .state
        .plan_retention(
            "young",
            &SemanticRevisionRetentionPolicy::default(),
            100_000,
        )
        .unwrap();
    assert!(young.records.is_empty());
    let age = project
        .state
        .plan_retention(
            "old",
            &SemanticRevisionRetentionPolicy::Age {
                history_age_seconds: 1,
            },
            100_000,
        )
        .unwrap();
    assert_eq!(age.records.len(), 11);
}

#[test]
fn unrelated_writes_do_not_consume_another_records_revision_budget() {
    let (mut project, revisions) = source_history(12);
    let request = project.edit(
        "update-b",
        vec![element("a", "a.rs", "A11"), element("b", "b.rs", "B1")],
    );
    project.publish(&request);
    let plan = project
        .state
        .plan_retention(
            "per-record",
            &SemanticRevisionRetentionPolicy::RevisionCount { revisions: 2 },
            100_000,
        )
        .unwrap();
    assert_eq!(
        plan.records
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        revisions[..10].iter().cloned().collect()
    );
    assert!(matches!(
        project
            .state
            .resolve(&SemanticRecordRef::element("b"))
            .unwrap()
            .body,
        SemanticRevisionBody::Element(_)
    ));
}

#[test]
fn pins_and_required_evidence_override_the_count_and_receipts_replay_after_collection() {
    let (mut project, revisions) = source_history(12);
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "pin-oldest".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "public".into(),
        source_context: "git:main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![revisions[0].clone()],
        connection_ids: vec![],
        source_anchor: None,
    };
    let accepted = project
        .state
        .plan_selection(&pin, &timestamp(20_000), project.state.commit_version + 1)
        .unwrap();
    project.state.apply_publication(accepted).unwrap();
    project.now = 21_000;
    let mut note = artifact("proof", &revisions[11].record_id, "Exact evidence");
    note.dependencies.push(ArtifactDependency {
        target: ArtifactDependencyTarget::SemanticElement {
            semantic_element_id: revisions[1].record_id.clone(),
        },
    });
    let mut edit = project.edit("save-proof", vec![]);
    edit.structure = None;
    edit.artifacts.push(SemanticArtifactRevisionWrite {
        checkpoint: false,
        artifact: note,
        media_type: "text/plain".into(),
    });
    project.publish(&edit);
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 2 };
    let plan = project
        .state
        .plan_retention("protected", &policy, 100_000)
        .unwrap();
    assert_eq!(plan.records.len(), 8);
    assert!(!plan.records.contains(&revisions[0]));
    assert!(!plan.records.contains(&revisions[1]));
    let receipt = project
        .state
        .apply_retention(&plan, &policy, 100_001, project.state.commit_version + 1)
        .unwrap();
    assert_eq!(
        project
            .state
            .apply_retention(&plan, &policy, 200_000, project.state.commit_version + 1)
            .unwrap(),
        receipt
    );
    assert!(matches!(
        project.state.resolve(&revisions[2]).unwrap().body,
        SemanticRevisionBody::Pruned
    ));
    assert!(matches!(
        project.state.resolve(&revisions[1]).unwrap().body,
        SemanticRevisionBody::Element(_)
    ));
}

#[test]
fn artifact_revisions_have_their_own_count_budget() {
    let mut project = ProjectFixture::new();
    project.initial();
    let mut revisions = vec![];
    for version in 0..12 {
        let mut request = project.edit(
            &format!("note-{version}"),
            vec![
                element("a", "a.rs", &format!("A{version}")),
                element("b", "b.rs", "B1"),
            ],
        );
        request.artifacts.push(SemanticArtifactRevisionWrite {
            checkpoint: false,
            artifact: artifact("note", "a", &format!("N{version}")),
            media_type: "text/plain".into(),
        });
        revisions.push(SemanticRecordRef::artifact(accepted_id(
            &project.publish(&request),
            "note",
        )));
    }
    let plan = project
        .state
        .plan_retention(
            "notes",
            &SemanticRevisionRetentionPolicy::RevisionCount { revisions: 10 },
            100_000,
        )
        .unwrap();
    assert_eq!(
        plan.records
            .into_iter()
            .filter(|record| record.kind == SemanticRecordKind::Artifact)
            .collect::<std::collections::BTreeSet<_>>(),
        revisions[..2].iter().cloned().collect()
    );
}

#[test]
fn malformed_ancestry_and_collection_against_a_different_project_fail_without_mutation() {
    let (mut project, _) = source_history(3);
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let plan = project
        .state
        .plan_retention("identity", &policy, 100_000)
        .unwrap();
    project.state.project_id = "another-project".into();
    let before = project.state.clone();
    assert!(
        project
            .state
            .apply_retention(&plan, &policy, 100_001, project.state.commit_version + 1)
            .unwrap_err()
            .to_string()
            .contains(PROJECT_ID)
    );
    assert_eq!(project.state, before);
    project.state.project_id = PROJECT_ID.into();
    let mut reverse = project.state.revision_links[0].clone();
    std::mem::swap(&mut reverse.older, &mut reverse.newer);
    project.state.revision_links.push(reverse);
    assert!(
        project
            .state
            .plan_retention("cycle", &policy, 100_000)
            .unwrap_err()
            .to_string()
            .contains("acyclic")
    );
}

fn advance_branch(project: &mut ProjectFixture, operation: &str, body: &str) {
    let mut request = project.edit(operation, vec![element("a", "a.rs", body)]);
    request.selection_id = "branch".into();
    request.source_context = "git:branch".into();
    request.expected = project
        .state
        .selection("branch")
        .unwrap()
        .expectation(&project.state.records);
    project.publish(&request);
}

#[test]
fn a_shorter_branch_preserves_its_own_budget_without_timestamp_ordering() {
    let (mut project, revisions) = source_history(12);
    let branch = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "branch-from-old".into(),
        project_root: ROOT.into(),
        project_id: PROJECT_ID.into(),
        selection_id: "branch".into(),
        source_context: "git:branch".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::FollowAccepted,
        records: vec![revisions[0].clone()],
        connection_ids: vec![],
        source_anchor: None,
    };
    let plan = project
        .state
        .plan_selection(
            &branch,
            &timestamp(13_000),
            project.state.commit_version + 1,
        )
        .unwrap();
    project.state.apply_publication(plan).unwrap();
    advance_branch(&mut project, "branch-1", "B1");
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 2 };
    let plan = project
        .state
        .plan_retention("short-branch", &policy, 100_000)
        .unwrap();
    assert!(!plan.records.contains(&revisions[0]));
    advance_branch(&mut project, "branch-2", "B2");
    let plan = project
        .state
        .plan_retention("longer-branch", &policy, 100_000)
        .unwrap();
    assert!(plan.records.contains(&revisions[0]));
}
