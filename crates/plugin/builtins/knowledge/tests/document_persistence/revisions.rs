//! Exact document bodies and retry behavior through the real Knowledge → Semantic boundary.
use super::*;

fn edit(document: &Value, content: &str) -> Value {
    json!({"document_id":document["document_id"], "project_root":document["project_root"],
        "expected_semantic_element_id":document["semantic_element_id"],
        "expected_revision":document["revision"], "content":content})
}

fn exact(host: &DocumentBoundaryHost, id: &str) -> lumvise_db_core::SemanticElement {
    let SemanticResult::Element(Some(element)) = host.execute(SemanticOperation::Element {
        semantic_element_id: id.into(),
    }) else {
        panic!("expected exact document locator `{id}`");
    };
    element
}

#[test]
fn document_updates_link_revisions_and_preserve_the_exact_old_payload() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let original = exact(&host, first["semantic_element_id"].as_str().unwrap());
    let changed = host
        .knowledge("update_document", edit(&first, "second body"))
        .unwrap();
    assert_ne!(
        changed["document"]["semantic_element_id"],
        first["semantic_element_id"]
    );
    assert_eq!(exact(&host, &original.semantic_element_id), original);
    assert_eq!(original.revision_dependencies.len(), 1);
    let SemanticResult::Artifact(Some(body)) = host.execute(SemanticOperation::Artifact {
        artifact_id: original.revision_dependencies[0].record_id.clone(),
    }) else {
        panic!("expected original document payload");
    };
    assert_eq!(body.content.as_deref(), Some("# A standalone report"));
    assert_eq!(host.knowledge("get_document", identity()).unwrap(), changed);
}

#[test]
fn accepted_document_reply_replays_after_reopen_and_another_edit() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let request = edit(&first, "second body");
    host.drop_document_reply = true;
    assert_eq!(
        host.knowledge("update_document", request.clone())
            .unwrap_err()
            .code,
        "lost_document_reply"
    );
    let frozen = host.publication_inputs.last().unwrap().clone();
    let mut reopened = DocumentBoundaryHost::with_persistence(host.persistence.clone());
    let accepted = reopened
        .knowledge("update_document", request.clone())
        .unwrap();
    assert_eq!(reopened.publication_inputs.last(), Some(&frozen));
    let later = reopened
        .knowledge("update_document", edit(&accepted["document"], "third body"))
        .unwrap();
    assert_eq!(
        reopened.knowledge("update_document", request).unwrap(),
        accepted
    );
    assert_eq!(
        reopened.knowledge("get_document", identity()).unwrap(),
        later
    );
}

#[test]
fn an_interleaved_document_edit_conflicts_without_publishing_the_stale_payload() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    host.interleaved_call = Some(("update_document", edit(&first, "other writer")));
    let error = host
        .knowledge("update_document", edit(&first, "stale writer"))
        .unwrap_err();
    assert_eq!(error.code, "knowledge_revision_conflict");
    assert_eq!(
        host.knowledge("get_document", identity()).unwrap()["document"]["content"],
        "other writer"
    );
}

pub(super) fn deletion(document: &Value) -> Value {
    json!({"project_root":document["project_root"], "document_id":document["document_id"],
        "expected_semantic_element_id":document["semantic_element_id"],
        "expected_revision":document["revision"]})
}

#[test]
fn deletion_requires_the_observed_revision_and_recreation_does_not_reuse_its_counter() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let changed = host
        .knowledge("update_document", edit(&first, "second body"))
        .unwrap()["document"]
        .clone();
    assert_eq!(
        host.knowledge("delete_document", deletion(&first))
            .unwrap_err()
            .code,
        "document_revision_conflict"
    );
    let request = deletion(&changed);
    let removed = host.knowledge("delete_document", request.clone()).unwrap();
    assert_eq!(
        host.knowledge("get_document", identity()).unwrap_err().code,
        "document_not_found"
    );
    let mut create = report();
    create["operation_id"] = json!("recreate-document");
    let recreated = host.knowledge("create_document", create).unwrap()["document"].clone();
    assert_eq!(recreated["revision"], 4);
    assert_ne!(
        recreated["semantic_element_id"],
        first["semantic_element_id"]
    );
    assert_eq!(host.knowledge("delete_document", request).unwrap(), removed);
    assert_eq!(
        host.knowledge("get_document", identity()).unwrap()["document"],
        recreated
    );
}

#[test]
fn deletion_leaves_a_small_identity_without_permanently_rooting_the_old_payload() {
    let mut host = DocumentBoundaryHost::new();
    let created = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let original = exact(&host, created["semantic_element_id"].as_str().unwrap());
    host.knowledge("delete_document", deletion(&created))
        .unwrap();
    let SemanticResult::RevisionHistory(history) =
        host.execute(SemanticOperation::RevisionHistory {
            project_root: "/project".into(),
        })
    else {
        panic!("expected revision history");
    };
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::Age {
        history_age_seconds: 1,
    };
    let plan = history
        .plan_retention("document-body-gc", &policy, i64::MAX / 2)
        .unwrap();
    assert!(plan.records.contains(&original.revision_dependencies[0]));
    let head = history.selection("working").unwrap();
    let tombstone = head
        .records
        .iter()
        .filter(|record| record.kind == lumvise_db_core::SemanticRecordKind::Element)
        .find_map(|record| {
            history
                .resolve(record)
                .filter(|resolved| match &resolved.body {
                    lumvise_db_core::SemanticRevisionBody::Element(element) => {
                        element.path == original.path
                    }
                    _ => false,
                })
        })
        .unwrap();
    let lumvise_db_core::SemanticRevisionBody::Element(element) = tombstone.body else {
        unreachable!();
    };
    assert!(element.revision_dependencies.is_empty());
    assert!(
        !serde_json::to_string(&element)
            .unwrap()
            .contains("# A standalone report")
    );
}

#[test]
fn accepted_deletion_replays_after_recreation_and_collection_of_the_old_marker() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let path = exact(&host, first["semantic_element_id"].as_str().unwrap()).path;
    let request = deletion(&first);
    let receipt = host.knowledge("delete_document", request.clone()).unwrap();
    let SemanticResult::RevisionHistory(history) =
        host.execute(SemanticOperation::RevisionHistory {
            project_root: "/project".into(),
        })
    else {
        panic!("expected revision history");
    };
    let marker = history
        .selection("working")
        .unwrap()
        .records
        .iter()
        .find(|record| {
            history.resolve(record).is_some_and(|revision| {
                matches!(revision.body,
            lumvise_db_core::SemanticRevisionBody::Element(ref element)
                if element.path == path)
            })
        })
        .unwrap()
        .clone();
    let mut create = report();
    create["operation_id"] = json!("recreate-before-collection");
    let recreated = host.knowledge("create_document", create).unwrap();
    let policy = lumvise_db_core::SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let SemanticResult::RevisionRetentionPlan(plan) =
        host.execute(SemanticOperation::PlanRevisionRetention {
            project_root: "/project".into(),
            operation_id: "collect-old-marker".into(),
            policy: policy.clone(),
        })
    else {
        panic!("expected a collection plan");
    };
    assert!(plan.records.contains(&marker));
    host.execute(SemanticOperation::CollectRevisionRetention { plan, policy });
    let SemanticResult::RevisionResolution(Some(retained)) =
        host.execute(SemanticOperation::ResolveRevision {
            project_root: "/project".into(),
            record: marker,
        })
    else {
        panic!("expected retained navigation metadata");
    };
    assert!(matches!(
        retained.body,
        lumvise_db_core::SemanticRevisionBody::Pruned
    ));
    assert_eq!(host.knowledge("delete_document", request).unwrap(), receipt);
    assert_eq!(
        host.knowledge("get_document", identity()).unwrap(),
        recreated
    );
}

#[test]
fn different_projects_can_use_the_same_logical_document_id_without_aliasing_bodies() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let mut other = report();
    other["project_root"] = json!("/other-project");
    other["content"] = json!("other project body");
    let second = host.knowledge("create_document", other).unwrap()["document"].clone();
    assert_ne!(first["semantic_element_id"], second["semantic_element_id"]);
    assert_eq!(
        host.knowledge("get_document", identity()).unwrap()["document"],
        first
    );
}

#[test]
fn two_first_documents_can_share_the_racing_root_creation_without_overwriting() {
    let mut host = DocumentBoundaryHost::new();
    let mut other = report();
    other["document_id"] = json!("other-document");
    other["content"] = json!("Other document content");
    host.interleaved_call = Some(("create_document", other));
    let first = host.knowledge("create_document", report()).unwrap();
    assert_eq!(host.knowledge("get_document", identity()).unwrap(), first);
    let listed = host
        .knowledge("list_documents", json!({"project_root":"/project"}))
        .unwrap();
    assert_eq!(listed["total"], 2);
}

#[test]
fn accepted_document_replay_fences_the_original_lineage_before_returning_its_body() {
    let mut host = DocumentBoundaryHost::new();
    host.knowledge("create_document", report()).unwrap();
    let publications = host.publication_inputs.len();
    host.reported_project_identity = Some("different-lineage".into());
    assert_eq!(
        host.knowledge("create_document", report())
            .unwrap_err()
            .code,
        "knowledge_project_mismatch"
    );
    assert_eq!(host.publication_inputs.len(), publications);
}

#[test]
fn a_canvas_edit_keeps_its_resource_identity_and_exact_old_document_body() {
    let mut host = DocumentBoundaryHost::new();
    let mut request = report();
    request["document_kind"] = json!("canvas");
    let created = host.knowledge("create_document", request).unwrap()["document"].clone();
    let changed = host.knowledge("update_document", json!({"project_root":"/project", "document_id":created["document_id"],
        "expected_semantic_element_id":created["semantic_element_id"],
        "expected_revision":created["revision"], "title":"Renamed board", "content":"updated scene"})).unwrap()["document"].clone();
    assert_eq!(
        changed["attachment_owner_id"],
        created["attachment_owner_id"]
    );
    let old = exact(&host, created["semantic_element_id"].as_str().unwrap());
    let current = exact(&host, changed["semantic_element_id"].as_str().unwrap());
    assert_ne!(old.revision_dependencies, current.revision_dependencies);
    let SemanticResult::Artifact(Some(body)) = host.execute(SemanticOperation::Artifact {
        artifact_id: current.revision_dependencies[0].record_id.clone(),
    }) else {
        panic!("expected updated document body");
    };
    assert_eq!(body.dependencies.len(), 1);
    assert_eq!(
        json!(body.dependencies[0].target)["artifact_id"],
        created["attachment_owner_id"]
    );
}

#[test]
fn an_old_counter_cannot_overwrite_a_different_locator_after_explicit_selection_changes() {
    let mut host = DocumentBoundaryHost::new();
    let first = host.knowledge("create_document", report()).unwrap()["document"].clone();
    let SemanticResult::RevisionHistory(history) =
        host.execute(SemanticOperation::RevisionHistory {
            project_root: "/project".into(),
        })
    else {
        panic!("expected document history");
    };
    let selected = history.selection("working").unwrap();
    let roots = selected
        .records
        .iter()
        .filter(|record| {
            record.kind == lumvise_db_core::SemanticRecordKind::Element
                && record.record_id != first["semantic_element_id"].as_str().unwrap()
        })
        .collect::<Vec<_>>();
    let update = serde_json::from_value(json!({"operation_id":"deselect-document-slot", "project_root":"/project",
        "project_id":history.project_id, "selection_id":"working", "source_context":"local", "mode":"follow_accepted",
        "expected_generation":selected.selection.generation, "records":roots, "connection_ids":[], "source_anchor":null})).unwrap();
    host.execute(SemanticOperation::SelectRevisions(update));
    let mut replacement = report();
    replacement["operation_id"] = json!("create-a-different-locator");
    replacement["content"] = json!("Different current document");
    let current = host.knowledge("create_document", replacement).unwrap()["document"].clone();
    assert_eq!(current["revision"], first["revision"]);
    assert_ne!(current["semantic_element_id"], first["semantic_element_id"]);
    assert_eq!(current["content"], "Different current document");
    assert_eq!(
        host.knowledge("update_document", edit(&first, "stale body"))
            .unwrap_err()
            .code,
        "document_revision_conflict"
    );
    assert_eq!(
        host.knowledge("delete_document", deletion(&first))
            .unwrap_err()
            .code,
        "document_revision_conflict"
    );
    assert_eq!(
        host.knowledge("get_document", identity()).unwrap()["document"],
        current
    );
}
