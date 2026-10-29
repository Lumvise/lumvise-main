//! Independent SDK brokers share publication state; only this throwaway database is collected.
use super::*;
use lumvise_db_core::{
    SemanticRecordRef, SemanticRevisionPublication, SemanticRevisionPublicationOutcome,
    SemanticRevisionRetentionPolicy, SemanticRevisionSelectionQuery,
    SemanticRevisionSelectionUpdate, SemanticRevisionStructure, SemanticRevisionStructureCoverage,
    SemanticSelectionMode,
};

const ROOT: &str = "/artifact-project";

fn advance_owner(host: &ArtifactBoundaryHost, source: &str, operation: &str) -> String {
    let SemanticResult::ProjectIdentity(identity) =
        host.execute(SemanticOperation::ProjectIdentity {
            project_root: ROOT.into(),
        })
    else {
        panic!("expected lineage")
    };
    let SemanticResult::RevisionObservation(Some(observed)) = host.execute(
        SemanticOperation::RevisionSelectionMembers(SemanticRevisionSelectionQuery {
            project_root: ROOT.into(),
            selection_id: "working".into(),
            structure_coverage: SemanticRevisionStructureCoverage::Project,
            artifact_ids: vec![],
        }),
    ) else {
        panic!("expected original structure basis")
    };
    let SemanticResult::Element(Some(mut owner)) = host.execute(SemanticOperation::Element {
        semantic_element_id: source.into(),
    }) else {
        panic!("expected original owner")
    };
    owner.name = format!("Source {operation}");
    let request = SemanticRevisionPublication {
        operation_id: operation.into(),
        project_root: ROOT.into(),
        project_id: identity.project_id,
        selection_id: "working".into(),
        source_context: "local".into(),
        producer_base: None,
        expected: observed.expected,
        structure: Some(SemanticRevisionStructure {
            partition: None,
            elements: vec![owner],
            relationships: vec![],
        }),
        artifacts: vec![],
        removed_artifact_ids: vec![],
        source_anchor: None,
    };
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(receipt)) =
        host.execute(SemanticOperation::PublishRevisions(request))
    else {
        panic!("expected source acceptance")
    };
    receipt
        .remaps
        .iter()
        .find(|remap| remap.incoming == SemanticRecordRef::element(source))
        .unwrap()
        .accepted
        .record_id
        .clone()
}

fn read_artifact(host: &mut ArtifactBoundaryHost, id: &str) -> Value {
    host.knowledge("get_knowledge", json!({"artifact_id":id}))
        .unwrap()["artifact"]
        .clone()
}

fn update_body(host: &mut ArtifactBoundaryHost, observed: &Value, content: &str) -> Value {
    host.knowledge("update_knowledge", update_request(observed, content))
        .unwrap()["artifact"]
        .clone()
}

fn fork_on_new_source(host: &mut ArtifactBoundaryHost, observed: &Value, operation: &str) -> Value {
    let owner = advance_owner(
        host,
        observed["semantic_element_id"].as_str().unwrap(),
        operation,
    );
    let mut edit = update_request(observed, operation);
    edit["semantic_element_id"] = json!(owner);
    let result = host.knowledge("update_knowledge", edit).unwrap()["artifact"].clone();
    assert_ne!(result["artifact_id"], observed["artifact_id"]);
    result
}

fn cite_old_artifact(host: &mut ArtifactBoundaryHost, owner: &str, original: &Value) {
    let mut request = create_request();
    request["artifact_id"] = json!("historical-citation");
    request["semantic_element_id"] = json!(owner);
    request["dependencies"] = json!([{"target":{"target_kind":"artifact","artifact_id":original["artifact_id"],"edit_version":original["edit_version"]}}]);
    host.knowledge("create_knowledge", request).unwrap();
}

fn collect_unneeded_history(host: &ArtifactBoundaryHost, unneeded: &str) {
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: ROOT.into(),
            operation_id: "isolated-cooperation-sweep".into(),
            policy: policy.clone(),
        })
    else {
        panic!("expected retention plan")
    };
    assert!(
        plan.records
            .iter()
            .all(|record| record != &SemanticRecordRef::artifact("spec"))
    );
    assert!(
        plan.records
            .contains(&SemanticRecordRef::artifact(unneeded)),
        "{plan:?}"
    );
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
}

fn select_original_pin(
    host: &ArtifactBoundaryHost,
    original: &Value,
    generation: i64,
    keep: bool,
) -> i64 {
    let SemanticResult::ProjectIdentity(identity) =
        host.execute(SemanticOperation::ProjectIdentity {
            project_root: ROOT.into(),
        })
    else {
        panic!("expected lineage")
    };
    let request = SemanticRevisionSelectionUpdate {
        operation_id: if keep {
            "pin-original"
        } else {
            "release-original"
        }
        .into(),
        project_root: ROOT.into(),
        project_id: identity.project_id,
        selection_id: "explicit-history".into(),
        source_context: "local".into(),
        expected_generation: generation,
        mode: SemanticSelectionMode::Pinned,
        records: if keep {
            vec![SemanticRecordRef::artifact("spec")]
        } else {
            vec![]
        },
        artifact_edits: if keep {
            std::collections::BTreeMap::from([(
                "spec".into(),
                original["edit_version"].as_u64().unwrap(),
            )])
        } else {
            Default::default()
        },
        connection_ids: vec![],
        source_anchor: None,
    };
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(receipt)) =
        host.execute(SemanticOperation::SelectRevisions(request))
    else {
        panic!("expected explicit pin acceptance")
    };
    receipt.selection.generation
}

fn cooperate(
    first: &mut ArtifactBoundaryHost,
    second: &mut ArtifactBoundaryHost,
) -> (Value, Value) {
    let original = first
        .knowledge("create_knowledge", create_request())
        .unwrap()["artifact"]
        .clone();
    let old_body = first.exact_artifact("spec");
    let owner = advance_owner(second, "owner", "source-v2");
    assert_ne!(owner, "owner");
    assert_eq!(read_artifact(first, "spec"), original);
    let shared = first
        .invoke_host(
            "storage.semantic",
            json!({"operation":"artifacts_for_elements","semantic_element_ids":[owner]}),
        )
        .unwrap();
    assert_eq!(
        shared["artifacts"][0]["artifact_id"], original["artifact_id"],
        "{shared}"
    );
    let inherited = first
        .knowledge(
            "list_knowledge",
            json!({"project_root":ROOT,"semantic_element_ids":[owner]}),
        )
        .unwrap();
    assert_eq!(
        inherited["artifacts"][0]["artifact_id"], original["artifact_id"],
        "{inherited}"
    );
    let searched = first
        .knowledge(
            "search_knowledge",
            json!({"project_root":ROOT,"semantic_element_id":owner,"query":"first body"}),
        )
        .unwrap();
    assert_eq!(
        searched["results"][0]["artifact_id"], original["artifact_id"],
        "{searched}"
    );
    let unrelated = first
        .knowledge(
            "search_knowledge",
            json!({"project_root":ROOT,"semantic_element_id":"not-an-owner","query":"first body"}),
        )
        .unwrap();
    assert_eq!(unrelated["results"], json!([]), "{unrelated}");
    let mut edit = update_request(&original, "New source explanation");
    edit["semantic_element_id"] = json!(owner);
    let next = second.knowledge("update_knowledge", edit).unwrap()["artifact"].clone();
    assert_ne!(next["artifact_id"], original["artifact_id"]);
    assert_eq!(next["semantic_element_id"], owner);
    assert_eq!(first.exact_artifact("spec"), old_body);
    let latest = update_body(second, &next, "Refined explanation");
    assert_eq!(latest["artifact_id"], next["artifact_id"]);
    assert_eq!(
        first
            .knowledge(
                "update_knowledge",
                update_request(&original, "Stale overwrite")
            )
            .unwrap_err()
            .code,
        "knowledge_revision_conflict"
    );
    let retired_id = latest["artifact_id"].as_str().unwrap().to_owned();
    let middle = fork_on_new_source(second, &latest, "source-v3");
    let latest = fork_on_new_source(second, &middle, "source-v4");
    let pin_generation = select_original_pin(first, &original, 0, true);
    cite_old_artifact(
        second,
        latest["semantic_element_id"].as_str().unwrap(),
        &original,
    );
    select_original_pin(first, &original, pin_generation, false);
    collect_unneeded_history(second, &retired_id);
    assert_eq!(read_artifact(first, "spec"), original);
    assert_eq!(
        read_artifact(first, latest["artifact_id"].as_str().unwrap()),
        latest
    );
    (original, latest)
}

#[test]
fn independent_brokers_preserve_cited_source_history_and_one_working_state_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cooperation.sqlite");
    let (original, latest) = {
        let persistence = Arc::new(LocalPersistence::open(&path).unwrap());
        let mut first = ArtifactBoundaryHost::with_persistence(persistence.clone());
        first.seed_owner();
        let mut second = ArtifactBoundaryHost::with_persistence(persistence);
        cooperate(&mut first, &mut second)
    };
    let mut reopened =
        ArtifactBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    assert_eq!(read_artifact(&mut reopened, "spec"), original);
    assert_eq!(
        read_artifact(&mut reopened, latest["artifact_id"].as_str().unwrap()),
        latest
    );
}
