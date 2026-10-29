//! Document citations must retain the artifact state the caller actually observed.
use super::*;

fn working_evidence(host: &mut DocumentBoundaryHost) -> Value {
    let owner = host.knowledge("create_document", report()).unwrap()["document"].clone();
    host.knowledge(
        "create_knowledge",
        json!({"artifact_id":"evidence",
        "semantic_element_id":owner["semantic_element_id"],"project_root":"/project",
        "knowledge_type":"specification","title":"Evidence","content":"Observed evidence"}),
    )
    .unwrap()["artifact"]
        .clone()
}

fn citing_document(evidence: &Value) -> Value {
    let mut request = report();
    request["document_id"] = json!("citing-report");
    request["references"] = json!([{"target_kind":"artifact","artifact_id":evidence["artifact_id"],
        "edit_version":evidence["edit_version"]}]);
    request
}

fn revise_evidence(host: &mut DocumentBoundaryHost, original: &Value) -> Value {
    host.knowledge(
        "update_knowledge",
        json!({"artifact_id":original["artifact_id"],
        "project_root":"/project","expected_revision":original["revision"],
        "content":"Later evidence"}),
    )
    .unwrap()["artifact"]
        .clone()
}

#[test]
fn an_exact_document_citation_freezes_the_observed_working_artifact() {
    let mut host = DocumentBoundaryHost::new();
    let original = working_evidence(&mut host);
    let citation = host
        .knowledge("create_document", citing_document(&original))
        .unwrap();
    let next = revise_evidence(&mut host, &original);
    assert_ne!(next["artifact_id"], original["artifact_id"]);
    assert_eq!(
        host.knowledge(
            "get_knowledge",
            json!({"artifact_id":original["artifact_id"]})
        )
        .unwrap()["artifact"]["content"],
        "Observed evidence"
    );
    assert_eq!(
        citation["document"]["references"][0]["edit_version"],
        original["edit_version"]
    );
}

#[test]
fn cited_evidence_survives_collection_of_obsolete_document_revisions() {
    let mut host = DocumentBoundaryHost::new();
    let original = working_evidence(&mut host);
    let mut citing = host
        .knowledge("create_document", citing_document(&original))
        .unwrap()["document"]
        .clone();
    revise_evidence(&mut host, &original);
    for content in [
        "Revised explanation",
        "Final explanation",
        "Reviewed explanation",
    ] {
        citing = host
            .knowledge(
                "update_document",
                json!({"project_root":"/project",
            "document_id":citing["document_id"],"expected_revision":citing["revision"],
            "expected_semantic_element_id":citing["semantic_element_id"],"content":content}),
            )
            .unwrap()["document"]
            .clone();
    }
    collect_obsolete_document_bodies(&host);
    let evidence = host
        .knowledge("get_knowledge", json!({"artifact_id":"evidence"}))
        .unwrap();
    assert_eq!(evidence["artifact"]["content"], original["content"]);
    assert_eq!(
        evidence["artifact"]["edit_version"],
        original["edit_version"]
    );
}

fn collect_obsolete_document_bodies(host: &DocumentBoundaryHost) {
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: "/project".into(),
            operation_id: "citation-collection".into(),
            policy: policy.clone(),
        })
    else {
        panic!("expected retention plan")
    };
    assert!(
        !plan.records.is_empty(),
        "expected obsolete document bodies to be eligible"
    );
    assert!(
        !plan
            .records
            .contains(&lumvise_db_core::SemanticRecordRef::artifact("evidence"))
    );
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
}

#[test]
fn a_stale_citation_token_cannot_save_a_document_against_newer_evidence() {
    let mut host = DocumentBoundaryHost::new();
    let original = working_evidence(&mut host);
    let next = revise_evidence(&mut host, &original);
    assert_eq!(next["artifact_id"], original["artifact_id"]);
    assert!(
        host.knowledge("create_document", citing_document(&original))
            .is_err()
    );
    assert!(
        host.knowledge(
            "get_document",
            json!({"project_root":"/project","document_id":"citing-report"})
        )
        .is_err()
    );
    assert_eq!(
        host.knowledge("get_knowledge", json!({"artifact_id":"evidence"}))
            .unwrap()["artifact"],
        next
    );
}

#[test]
fn a_working_citation_needs_an_original_token_but_a_frozen_citation_does_not() {
    let mut host = DocumentBoundaryHost::new();
    let original = working_evidence(&mut host);
    let mut unversioned = citing_document(&original);
    unversioned["references"][0]
        .as_object_mut()
        .unwrap()
        .remove("edit_version");
    assert!(
        host.knowledge("create_document", unversioned.clone())
            .is_err()
    );
    host.knowledge("create_document", citing_document(&original))
        .unwrap();
    unversioned["document_id"] = json!("second-citation");
    assert!(host.knowledge("create_document", unversioned).is_ok());
}

#[test]
fn bulk_knowledge_reads_include_the_working_version_of_the_body_they_return() {
    let mut host = DocumentBoundaryHost::new();
    let original = working_evidence(&mut host);
    let listed = host
        .knowledge("list_knowledge", json!({"project_root":"/project"}))
        .unwrap();
    assert_eq!(
        listed["artifacts"][0]["edit_version"],
        original["edit_version"]
    );
    assert_eq!(listed["artifacts"][0]["content"], original["content"]);
}

#[test]
fn a_concurrent_edit_cannot_attach_its_new_token_to_an_older_listed_body() {
    let mut host = DocumentBoundaryHost::new();
    let original = working_evidence(&mut host);
    host.before_list_versions = Some(json!({"project_root":"/project","artifact_id":"evidence",
        "expected_revision":original["revision"],"content":"New evidence"}));
    assert!(
        host.knowledge("list_knowledge", json!({"project_root":"/project"}))
            .is_err()
    );
    let listed = host
        .knowledge("list_knowledge", json!({"project_root":"/project"}))
        .unwrap();
    assert_eq!(listed["artifacts"][0]["content"], "New evidence");
    assert_eq!(listed["artifacts"][0]["edit_version"], 2);
}
