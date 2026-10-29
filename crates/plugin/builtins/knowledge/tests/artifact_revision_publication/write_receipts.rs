//! Producer acknowledgements remain useful after exact artifact bodies are collected.
use super::*;

fn receipt(host: &mut ArtifactBoundaryHost, action: &str, operation: &str) -> Value {
    host.knowledge(
        "get_artifact_write_receipt",
        json!({"project_root":"/artifact-project",
        "action":action,"operation_id":operation}),
    )
    .unwrap()
}

fn create_with_identity(host: &mut ArtifactBoundaryHost, operation: &str) -> Value {
    let mut request = create_request();
    request["operation_id"] = json!(operation);
    host.knowledge("create_knowledge", request).unwrap()["artifact"].clone()
}

fn collect_artifact(host: &ArtifactBoundaryHost, artifact: &Value) {
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: "/artifact-project".into(),
            operation_id: "artifact-receipt-gc".into(),
            policy: policy.clone(),
        })
    else {
        panic!("retention plan expected")
    };
    let record =
        lumvise_db_core::SemanticRecordRef::artifact(artifact["artifact_id"].as_str().unwrap());
    assert!(plan.records.contains(&record));
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
    let SemanticResult::RevisionResolution(Some(resolution)) =
        host.execute(SemanticOperation::ResolveRevision {
            project_root: "/artifact-project".into(),
            record,
        })
    else {
        panic!("retained revision header expected")
    };
    assert!(matches!(
        resolution.body,
        lumvise_db_core::SemanticRevisionBody::Pruned
    ));
}

#[test]
fn artifact_acknowledgement_survives_collection_and_another_instance() {
    let mut host = ArtifactBoundaryHost::new();
    let original = create_with_identity(&mut host, "artifact-original");
    let accepted = receipt(&mut host, "create", "artifact-original");
    checkpoint_artifact(&host, original["artifact_id"].as_str().unwrap());
    host.knowledge("update_knowledge", update_request(&original, "new content"))
        .unwrap();
    collect_artifact(&host, &original);
    let mut other = ArtifactBoundaryHost::with_persistence(host.persistence.clone());
    assert_eq!(receipt(&mut other, "create", "artifact-original"), accepted);
    assert_eq!(
        accepted["artifact_revision"]["artifact_id"],
        original["artifact_id"]
    );
    assert_eq!(accepted["lifecycle_revision"], original["revision"]);
    assert_eq!(accepted["state"], "accepted");
    assert!(other.publication_inputs.is_empty());
    assert!(!accepted.to_string().contains("first body"));
}

#[test]
fn reading_a_pending_artifact_acknowledgement_never_resends_it() {
    let mut host = ArtifactBoundaryHost::new();
    let mut request = create_request();
    request["operation_id"] = json!("artifact-lost-reply");
    host.drop_publication_reply = true;
    assert_eq!(
        host.knowledge("create_knowledge", request.clone())
            .unwrap_err()
            .code,
        "lost_publication_reply"
    );
    let before = host.publication_inputs.len();
    assert_eq!(
        receipt(&mut host, "create", "artifact-lost-reply")["state"],
        "pending"
    );
    assert_eq!(host.publication_inputs.len(), before);
    host.knowledge("create_knowledge", request).unwrap();
    assert_eq!(
        receipt(&mut host, "create", "artifact-lost-reply")["state"],
        "accepted"
    );
}

#[test]
fn artifact_acknowledgements_preserve_conflicts_and_enforce_project_lineage() {
    let mut host = ArtifactBoundaryHost::new();
    let original = create_with_identity(&mut host, "artifact-before-conflict");
    let mut request = update_request(&original, "stale content");
    request["operation_id"] = json!("artifact-conflict");
    host.interleave_publication = true;
    assert_eq!(
        host.knowledge("update_knowledge", request)
            .unwrap_err()
            .code,
        "knowledge_revision_conflict"
    );
    assert_eq!(
        receipt(&mut host, "update", "artifact-conflict")["state"],
        "conflicted"
    );
    assert_eq!(
        receipt(&mut host, "create", "artifact-conflict")["state"],
        "not_found"
    );
    host.reported_project_identity = Some("other-lineage".into());
    let error = host
        .knowledge(
            "get_artifact_write_receipt",
            json!({"project_root":"/artifact-project",
        "operation_id":"artifact-before-conflict","action":"create"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "knowledge_project_mismatch");
}

#[test]
fn an_update_acknowledges_its_new_exact_id_and_original_counter_after_collection() {
    let mut host = ArtifactBoundaryHost::new();
    let original = create_with_identity(&mut host, "before-artifact-update");
    checkpoint_artifact(&host, original["artifact_id"].as_str().unwrap());
    let mut request = update_request(&original, "accepted second body");
    request["operation_id"] = json!("accepted-artifact-update");
    let accepted = host.knowledge("update_knowledge", request).unwrap()["artifact"].clone();
    checkpoint_artifact(&host, accepted["artifact_id"].as_str().unwrap());
    host.knowledge(
        "update_knowledge",
        update_request(&accepted, "later third body"),
    )
    .unwrap();
    collect_artifact(&host, &accepted);
    let result = receipt(&mut host, "update", "accepted-artifact-update");
    assert_eq!(
        result["artifact_revision"]["artifact_id"],
        accepted["artifact_id"]
    );
    assert_ne!(
        result["artifact_revision"]["artifact_id"],
        original["artifact_id"]
    );
    assert_eq!(result["lifecycle_revision"], 2);
}

#[test]
fn artifact_deletion_acknowledges_the_removed_id_without_inventing_a_body_counter() {
    let mut host = ArtifactBoundaryHost::new();
    let original = create_with_identity(&mut host, "before-artifact-deletion");
    host.knowledge(
        "delete_knowledge",
        json!({"project_root":"/artifact-project",
        "artifact_id":original["artifact_id"],"expected_revision":original["revision"],"operation_id":"delete-artifact"}),
    )
    .unwrap();
    let result = receipt(&mut host, "delete", "delete-artifact");
    assert_eq!(result["state"], "accepted");
    assert_eq!(
        result["artifact_revision"]["artifact_id"],
        original["artifact_id"]
    );
    assert_eq!(result["lifecycle_revision"], Value::Null);
}
