//! Selected-family lookup crosses the same public Local/resource boundary as publication.
mod common;
use common::{LocalPublicationServer, RevisionTestClient};
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::sync::Arc;

const ROOT: &str = "/families";

fn element(revision: u32) -> SemanticElement {
    SemanticElement {
        project_root: ROOT.into(),
        semantic_element_id: "file:a".into(),
        semantic_source_id: "files".into(),
        path: "a.rs".into(),
        element_kind: "file".into(),
        name: "a.rs".into(),
        parent_element_id: None,
        revision_dependencies: vec![],
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"revision":revision}),
    }
}

fn execute(storage: &dyn SemanticPersistence, operation: SemanticOperation) -> SemanticResult {
    storage
        .execute(operation, &InvocationControl::sixty_seconds())
        .unwrap()
}

fn publish(
    storage: &dyn SemanticPersistence,
    operation: &str,
    revision: u32,
) -> SemanticRevisionPublicationReceipt {
    let edit = RevisionTestClient::new(storage, ROOT, "main")
        .publication(operation, vec![element(revision)]);
    match execute(storage, SemanticOperation::PublishRevisions(edit)) {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt,
        other => panic!("{other:?}; expected publication acceptance"),
    }
}

fn families(storage: &dyn SemanticPersistence, selection: &str) -> Vec<SemanticRevisionRemap> {
    let query = SemanticRevisionFamilyQuery {
        project_root: ROOT.into(),
        selection_id: selection.into(),
        expected_commit_version: None,
        records: vec![
            SemanticRecordRef::element("file:a"),
            SemanticRecordRef::artifact("unknown"),
        ],
    };
    match execute(storage, SemanticOperation::RevisionFamilies(query)) {
        SemanticResult::RevisionFamilies(remaps) => remaps,
        other => panic!("{other:?}; expected explicit selected-family remaps"),
    }
}

#[test]
fn local_and_resource_clients_resolve_the_chosen_revision_without_changing_exact_reads() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let remote = CentralizedPersistence::new(
        Arc::new(LocalPublicationServer(local.clone())),
        "family-client",
    );
    publish(local.as_ref(), "initial", 1);
    let changed = publish(&remote, "changed", 2);
    let expected = vec![SemanticRevisionRemap {
        incoming: SemanticRecordRef::element("file:a"),
        accepted: changed.remaps[0].accepted.clone(),
    }];
    assert_eq!(families(local.as_ref(), "working"), expected);
    assert_eq!(families(&remote, "working"), expected);
    let SemanticResult::Element(Some(old)) = execute(
        &remote,
        SemanticOperation::Element {
            semantic_element_id: "file:a".into(),
        },
    ) else {
        panic!("expected the exact old body");
    };
    assert_eq!(old, element(1));
    assert!(families(&remote, "unknown-selection").is_empty());
}

#[test]
fn resource_family_reads_can_fence_a_previously_captured_view() {
    let local = Arc::new(LocalPersistence::in_memory().unwrap());
    let remote = CentralizedPersistence::new(
        Arc::new(LocalPublicationServer(local.clone())),
        "family-view",
    );
    let first = publish(local.as_ref(), "initial", 1);
    let next = publish(local.as_ref(), "changed", 2);
    let mut query = SemanticRevisionFamilyQuery {
        project_root: ROOT.into(),
        selection_id: "working".into(),
        expected_commit_version: Some(first.commit_version),
        records: vec![SemanticRecordRef::element("file:a")],
    };
    let error = SemanticPersistence::execute(
        &remote,
        SemanticOperation::RevisionFamilies(query.clone()),
        &InvocationControl::sixty_seconds(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("captured read commit"));
    query.expected_commit_version = Some(next.commit_version);
    assert!(
        matches!(execute(&remote, SemanticOperation::RevisionFamilies(query)), SemanticResult::RevisionFamilies(remaps) if remaps.len() == 1)
    );
}

#[test]
fn explicit_pins_and_withdrawal_determine_the_result() {
    let local = LocalPersistence::in_memory().unwrap();
    publish(&local, "initial", 1);
    let project_id = RevisionTestClient::new(&local, ROOT, "main").project_id();
    let pin = SemanticRevisionSelectionUpdate {
        artifact_edits: Default::default(),
        operation_id: "pin".into(),
        project_root: ROOT.into(),
        project_id,
        selection_id: "public".into(),
        source_context: "main".into(),
        expected_generation: 0,
        mode: SemanticSelectionMode::Pinned,
        records: vec![SemanticRecordRef::element("file:a")],
        connection_ids: vec![],
        source_anchor: None,
    };
    assert!(matches!(
        execute(&local, SemanticOperation::SelectRevisions(pin)),
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(_))
    ));
    publish(&local, "changed", 2);
    assert_eq!(
        families(&local, "public")[0].accepted,
        SemanticRecordRef::element("file:a")
    );
    let removal = RevisionTestClient::new(&local, ROOT, "main").publication("remove", vec![]);
    execute(&local, SemanticOperation::PublishRevisions(removal));
    assert!(families(&local, "working").is_empty());
    assert_eq!(
        families(&local, "public")[0].accepted,
        SemanticRecordRef::element("file:a")
    );
}

#[test]
fn header_lookup_survives_payload_collection_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("families.sqlite");
    let expected;
    {
        let local = LocalPersistence::open(&database).unwrap();
        publish(&local, "initial", 1);
        publish(&local, "changed", 2);
        expected = publish(&local, "changed-again", 3).remaps[0]
            .accepted
            .clone();
        let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
        let SemanticResult::RevisionRetentionPlan(plan) = execute(
            &local,
            SemanticOperation::PlanRevisionRetention {
                project_root: ROOT.into(),
                operation_id: "collect-unneeded".into(),
                policy: policy.clone(),
            },
        ) else {
            panic!("expected a retention plan");
        };
        assert!(plan.records.contains(&SemanticRecordRef::element("file:a")));
        execute(
            &local,
            SemanticOperation::CollectRevisionRetention { plan, policy },
        );
    }
    let reopened = LocalPersistence::open(&database).unwrap();
    assert_eq!(families(&reopened, "working")[0].accepted, expected);
    assert!(matches!(
        execute(
            &reopened,
            SemanticOperation::ResolveRevision {
                project_root: ROOT.into(),
                record: SemanticRecordRef::element("file:a")
            }
        ),
        SemanticResult::RevisionResolution(Some(SemanticRevisionResolution {
            body: SemanticRevisionBody::Pruned,
            ..
        }))
    ));
}
