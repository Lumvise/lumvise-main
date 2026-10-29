//! Known write acknowledgements survive source advances and actual payload collection.
use super::*;

const ROOT: &str = "/project";

fn create_request(operation: &str) -> Value {
    json!({"operation_id":operation,"project_root":ROOT,"document_id":"receipt-document",
        "document_kind":"ruleset","title":"Rules","content":"initial ruleset body"})
}

fn update_request(document: &Value, operation: &str, content: &str) -> Value {
    json!({"operation_id":operation,"project_root":ROOT,"document_id":document["document_id"],
        "expected_semantic_element_id":document["semantic_element_id"],
        "expected_revision":document["revision"],"content":content})
}

fn read_receipt(host: &mut DocumentBoundaryHost, action: &str, operation: &str) -> Value {
    host.knowledge(
        "get_document_write_receipt",
        json!({"project_root":ROOT,
        "action":action,"operation_id":operation}),
    )
    .unwrap()
}

fn created_document(host: &mut DocumentBoundaryHost, operation: &str) -> Value {
    host.knowledge("create_document", create_request(operation))
        .unwrap()["document"]
        .clone()
}

fn edit_document(
    host: &mut DocumentBoundaryHost,
    document: &Value,
    operation: &str,
    content: &str,
) -> Value {
    host.knowledge(
        "update_document",
        update_request(document, operation, content),
    )
    .unwrap()["document"]
        .clone()
}

fn collect_old_revisions(host: &DocumentBoundaryHost, old: &Value) {
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: ROOT.into(),
            operation_id: "collect-write-receipt-test".into(),
            policy: policy.clone(),
        })
    else {
        panic!("native collection plan expected");
    };
    let record =
        lumvise_db_core::SemanticRecordRef::element(old["semantic_element_id"].as_str().unwrap());
    assert!(plan.records.contains(&record));
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
    assert_collected(host, record);
}

fn assert_collected(host: &DocumentBoundaryHost, record: lumvise_db_core::SemanticRecordRef) {
    let SemanticResult::RevisionResolution(Some(resolution)) =
        host.execute(SemanticOperation::ResolveRevision {
            project_root: ROOT.into(),
            record,
        })
    else {
        panic!("retained revision header expected");
    };
    assert!(matches!(
        resolution.body,
        lumvise_db_core::SemanticRevisionBody::Pruned
    ));
}

#[test]
fn accepted_write_reference_survives_later_edits_collection_and_another_host() {
    let mut host = DocumentBoundaryHost::new();
    let original = created_document(&mut host, "create-original");
    let before = read_receipt(&mut host, "create", "create-original");
    edit_document(&mut host, &original, "later-edit", "new current rules");
    collect_old_revisions(&host, &original);
    let mut other = DocumentBoundaryHost::with_persistence(host.persistence.clone());
    let after = read_receipt(&mut other, "create", "create-original");
    assert_eq!(after, before);
    assert_eq!(after["state"], "accepted");
    assert_eq!(after["lifecycle_revision"], original["revision"]);
    assert_eq!(
        after["document_revision"]["semantic_element_id"],
        original["semantic_element_id"]
    );
    assert!(!after.to_string().contains("initial ruleset body"));
    assert!(other.publication_inputs.is_empty());
}

fn lose_update_acknowledgement(host: &mut DocumentBoundaryHost, original: &Value) -> Value {
    let frozen = update_request(original, "lost-update", "frozen user content");
    host.drop_document_reply = true;
    assert_eq!(
        host.knowledge("update_document", frozen.clone())
            .unwrap_err()
            .code,
        "lost_document_reply"
    );
    let publications_before_read = host.publication_inputs.len();
    assert_eq!(
        read_receipt(host, "update", "lost-update")["state"],
        "pending"
    );
    assert_eq!(host.publication_inputs.len(), publications_before_read);
    frozen
}

fn advance_and_collect_lost_update(host: &mut DocumentBoundaryHost) -> Value {
    let accepted = host
        .knowledge(
            "get_document",
            json!({"project_root":ROOT,"document_id":"receipt-document"}),
        )
        .unwrap()["document"]
        .clone();
    edit_document(host, &accepted, "newest-update", "current content");
    collect_old_revisions(host, &accepted);
    accepted
}

#[test]
fn an_accepted_but_lost_document_reply_is_recoverable_without_its_collected_body() {
    let mut host = DocumentBoundaryHost::new();
    let original = created_document(&mut host, "create-for-lost-reply");
    let frozen = lose_update_acknowledgement(&mut host, &original);
    let accepted = advance_and_collect_lost_update(&mut host);
    assert_eq!(
        host.knowledge("update_document", frozen).unwrap_err().code,
        "document_revision_unavailable"
    );
    let receipt = read_receipt(&mut host, "update", "lost-update");
    assert_eq!(receipt["state"], "accepted");
    assert_eq!(
        receipt["document_revision"]["semantic_element_id"],
        accepted["semantic_element_id"]
    );
    assert!(!receipt.to_string().contains("frozen user content"));
    assert_compact_acknowledgement(&mut host, &receipt);
}

fn assert_compact_acknowledgement(host: &mut DocumentBoundaryHost, receipt: &Value) {
    let stored = host
        .host_call(
            "storage.plugin",
            json!({"operation":"get_row",
        "table_name":"knowledge_publications","row_key":receipt["publication_id"]}),
        )
        .unwrap();
    assert_eq!(stored["row"]["value"]["state"], "accepted");
    assert!(stored["row"]["value"].get("publication").is_none());
    assert!(!stored.to_string().contains("frozen user content"));
}

fn assert_other_project_refused(host: &mut DocumentBoundaryHost, action: &str, operation: &str) {
    let error = host
        .knowledge(
            "get_document_write_receipt",
            json!({"project_root":"/other",
        "action":action,"operation_id":operation}),
        )
        .unwrap_err();
    assert_eq!(error.code, "knowledge_publication_scope_mismatch");
}

#[test]
fn unknown_or_different_action_does_not_publish_a_document() {
    let mut host = DocumentBoundaryHost::new();
    let missing = read_receipt(&mut host, "create", "unknown");
    assert_eq!(missing["state"], "not_found");
    assert_eq!(missing["document_revision"], Value::Null);
    assert!(host.publication_inputs.is_empty());
    host.knowledge("create_document", create_request("known-create"))
        .unwrap();
    let writes = host.publication_inputs.len();
    assert_eq!(
        read_receipt(&mut host, "update", "known-create")["state"],
        "not_found"
    );
    assert_eq!(host.publication_inputs.len(), writes);
}

#[test]
fn write_reference_is_bound_to_its_original_project() {
    let mut host = DocumentBoundaryHost::new();
    host.knowledge("create_document", create_request("scoped-create"))
        .unwrap();
    assert_other_project_refused(&mut host, "create", "scoped-create");
}

#[test]
fn an_original_deletion_reference_survives_recreation_and_collection() {
    let mut host = DocumentBoundaryHost::new();
    let original = created_document(&mut host, "create-for-delete");
    let mut deletion = revisions::deletion(&original);
    deletion["operation_id"] = json!("delete-original");
    host.knowledge("delete_document", deletion).unwrap();
    let before = read_receipt(&mut host, "delete", "delete-original");
    assert_eq!(before["state"], "accepted");
    host.knowledge("create_document", create_request("recreate-deleted"))
        .unwrap();
    collect_old_revisions(&host, &before["document_revision"]);
    assert_eq!(read_receipt(&mut host, "delete", "delete-original"), before);
}

#[test]
fn conflicted_write_receipts_keep_their_original_scope() {
    let mut host = DocumentBoundaryHost::new();
    let original = created_document(&mut host, "create-for-conflict");
    host.interleaved_call = Some((
        "update_document",
        update_request(&original, "other-host-edit", "other accepted body"),
    ));
    let stale = update_request(&original, "conflicted-edit", "stale user body");
    assert_eq!(
        host.knowledge("update_document", stale).unwrap_err().code,
        "knowledge_revision_conflict"
    );
    let receipt = read_receipt(&mut host, "update", "conflicted-edit");
    assert_eq!(receipt["state"], "conflicted");
    assert_eq!(receipt["document_revision"], Value::Null);
    assert_other_project_refused(&mut host, "update", "conflicted-edit");
}

#[test]
fn a_replaced_project_binding_cannot_read_an_old_acknowledgement() {
    let mut host = DocumentBoundaryHost::new();
    host.knowledge("create_document", create_request("bound-to-lineage"))
        .unwrap();
    host.reported_project_identity = Some("replacement-project".into());
    let error = host
        .knowledge(
            "get_document_write_receipt",
            json!({"project_root":ROOT,
        "action":"create","operation_id":"bound-to-lineage"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "knowledge_project_mismatch");
}

#[test]
fn another_checkout_route_can_read_the_same_durable_project_acknowledgement() {
    let mut host = DocumentBoundaryHost::new();
    let original = created_document(&mut host, "same-lineage");
    let identity = host
        .host_call(
            "storage.semantic",
            json!({"operation":"project_identity","project_root":ROOT}),
        )
        .unwrap();
    // The existing named host override models the selected adapter's project-route resolution.
    host.reported_project_identity =
        Some(identity["identity"]["project_id"].as_str().unwrap().into());
    let receipt = host
        .knowledge(
            "get_document_write_receipt",
            json!({"project_root":"/other-checkout",
        "action":"create","operation_id":"same-lineage"}),
        )
        .unwrap();
    assert_eq!(receipt["state"], "accepted");
    assert_eq!(receipt["project_root"], "/other-checkout");
    assert_eq!(receipt["document_revision"]["project_root"], ROOT);
    assert_eq!(
        receipt["document_revision"]["semantic_element_id"],
        original["semantic_element_id"]
    );
}
