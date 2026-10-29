//! Exercises the shared policy only through portable calls across the real network boundary.
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

use super::{
    client::*,
    processes::{event, wait_for_publication},
};

pub(super) fn run(role: &str) {
    match role {
        "seed" => seed(),
        "edit-a" => edit_a(),
        "edit-b" => edit_b(),
        "replay" => replay(),
        "artifact-history" => artifact_history(),
        "verify-collected" => verify_collected(),
        other => panic!("role `{other}`; expected a revision cooperation scenario"),
    }
    event("SCENARIO_DONE");
}

fn seed() {
    let client = client("client-a", false);
    let mut request = publication(
        &client,
        "initial-source",
        SemanticRevisionStructureCoverage::Project,
        vec![],
    );
    request.structure = Some(SemanticRevisionStructure {
        partition: None,
        elements: vec![source_element("a", "a.rs"), source_element("b", "b.rs")],
        relationships: vec![],
    });
    accepted(&client, request);
    assert_eq!(
        save_artifact(
            &client,
            "note",
            "a",
            "original source explanation",
            "initial-note"
        ),
        "note"
    );
}

fn edit_a() {
    let observed = client("client-a", false);
    let request = source_edit(&observed, "a.rs", 2);
    save_pending(request.clone());
    event("BASE_READY");
    wait_for_publication();
    let lossy = client("client-a", true);
    assert!(
        SemanticPersistence::execute(
            &lossy,
            SemanticOperation::PublishRevisions(request),
            &InvocationControl::sixty_seconds()
        )
        .is_err()
    );
    event("REPLY_DROPPED");
}

fn edit_b() {
    let client = client("client-b", false);
    let request = source_edit(&client, "b.rs", 2);
    event("BASE_READY");
    wait_for_publication();
    accepted(&client, request);
    event("WRITE_ACCEPTED");
}

fn replay() {
    let client = client("client-a", false);
    let request = pending();
    let before = selected(&client).commit_version;
    let first = accepted(&client, request.clone());
    assert_eq!(first, accepted(&client, request.clone()));
    assert_eq!(selected(&client).commit_version, before);
    let mut stale = request;
    stale.operation_id = "stale-source-a".into();
    assert!(matches!(
        execute(&client, SemanticOperation::PublishRevisions(stale)),
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Conflict(_))
    ));
    assert_sources(&client, 2);
}

fn assert_sources(client: &CentralizedPersistence, a_revision: u64) {
    assert_eq!(
        selected_element(client, "a.rs").metadata["revision"],
        a_revision
    );
    assert_eq!(selected_element(client, "b.rs").metadata["revision"], 2);
}

fn pin_original(client: &CentralizedPersistence, generation: i64, keep: bool) -> i64 {
    let SemanticResult::ProjectIdentity(identity) = execute(
        client,
        SemanticOperation::ProjectIdentity {
            project_root: ROOT.into(),
        },
    ) else {
        panic!("expected lineage")
    };
    let request = SemanticRevisionSelectionUpdate {
        operation_id: format!("pin-original-{keep}"),
        project_root: ROOT.into(),
        project_id: identity.project_id,
        selection_id: "explicit-evidence".into(),
        source_context: "local".into(),
        expected_generation: generation,
        artifact_edits: if keep {
            std::collections::BTreeMap::from([("note".into(), 1)])
        } else {
            Default::default()
        },
        mode: SemanticSelectionMode::Pinned,
        records: if keep {
            vec![SemanticRecordRef::artifact("note")]
        } else {
            vec![]
        },
        connection_ids: vec![],
        source_anchor: None,
    };
    match execute(client, SemanticOperation::SelectRevisions(request)) {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt.selection.generation,
        other => panic!("result `{other:?}`; expected pin acknowledgement"),
    }
}

fn cite_original(client: &CentralizedPersistence, owner: &str) {
    let mut request = publication(
        client,
        "cite-original",
        SemanticRevisionStructureCoverage::None,
        vec!["citation".into()],
    );
    let mut citation = artifact("citation", owner, "required exact earlier evidence");
    citation.artifact.title = "Historical citation".into();
    citation.artifact.dependencies = serde_json::from_value(json!([{"target":{
        "target_kind":"artifact", "artifact_id":"note", "edit_version":1
    }}]))
    .unwrap();
    request.artifacts.push(citation);
    accepted(client, request);
}

fn source_and_note(client: &CentralizedPersistence, prior: &str, revision: u64) -> String {
    accepted(client, source_edit(client, "a.rs", revision));
    let owner = selected_element(client, "a.rs").semantic_element_id;
    save_artifact(
        client,
        prior,
        &owner,
        &format!("source-v{revision} body"),
        &format!("note-v{revision}"),
    )
}

fn collect_unneeded(client: &CentralizedPersistence, retired: &str) {
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) = execute(
        client,
        SemanticOperation::PlanRevisionRetention {
            project_root: ROOT.into(),
            operation_id: "network-collection".into(),
            policy: policy.clone(),
        },
    ) else {
        panic!("expected retention plan")
    };
    assert!(!plan.records.contains(&SemanticRecordRef::artifact("note")));
    assert!(
        plan.records.contains(&SemanticRecordRef::artifact(retired)),
        "{plan:?}"
    );
    execute(
        client,
        SemanticOperation::CollectRevisionRetention { plan, policy },
    );
    assert!(matches!(
        resolution(client, retired).body,
        SemanticRevisionBody::Pruned
    ));
}

fn artifact_history() {
    let client = client("client-b", false);
    let original = resolution(&client, "note");
    assert!(original.artifact_state.as_ref().unwrap().frozen);
    let owner = selected_element(&client, "a.rs").semantic_element_id;
    let next = save_artifact(&client, "note", &owner, "source-v2 body", "note-v2");
    assert_ne!(next, "note");
    assert_eq!(
        next,
        save_artifact(
            &client,
            &next,
            &owner,
            "refined source-v2 body",
            "note-v2-refined"
        )
    );
    assert_eq!(resolution(&client, "note").body, original.body);
    let third = source_and_note(&client, &next, 3);
    source_and_note(&client, &third, 4);
    let pin = pin_original(&client, 0, true);
    cite_original(
        &client,
        &selected_element(&client, "a.rs").semantic_element_id,
    );
    pin_original(&client, pin, false);
    collect_unneeded(&client, &next);
    assert_eq!(resolution(&client, "note").body, original.body);
    assert_sources(&client, 4);
}

fn verify_collected() {
    let client = client("client-a", false);
    assert_sources(&client, 4);
    let SemanticRevisionBody::Artifact(original) = resolution(&client, "note").body else {
        panic!("expected cited body")
    };
    assert_eq!(
        original.content.as_deref(),
        Some("original source explanation")
    );
    let current = selected(&client)
        .artifacts
        .into_iter()
        .find(|row| row.title == "Working note")
        .unwrap();
    assert_eq!(current.content.as_deref(), Some("source-v4 body"));
}
