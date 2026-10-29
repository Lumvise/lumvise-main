//! Acknowledgement resolves only its immutable edges after later edits and collection.
mod common;
use common::{LocalPublicationServer, RevisionTestClient};
use lumvise_db_core::*;
use lumvise_resource_routing::InvocationControl;
use serde_json::json;
use std::sync::Arc;

const ROOT: &str = "/connection-reads";

fn execute(
    store: &dyn SemanticPersistence,
    operation: SemanticOperation,
) -> PersistenceResult<SemanticResult> {
    store.execute(operation, &InvocationControl::sixty_seconds())
}

fn source(id: &str, version: i64) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: ROOT.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: format!("{id}.rs"),
        name: id.into(),
        element_kind: "function".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({"version":version}),
    }
}

fn call() -> SemanticRelationship {
    SemanticRelationship {
        project_root: ROOT.into(),
        source_element_id: "a".into(),
        target_element_id: "b".into(),
        relationship_kind: "calls".into(),
        label: "calls".into(),
        metadata: json!({}),
    }
}

fn publish(
    store: &dyn SemanticPersistence,
    id: &str,
    version: i64,
) -> SemanticRevisionPublicationReceipt {
    let mut request = RevisionTestClient::new(store, ROOT, "local")
        .publication(id, vec![source("a", version), source("b", version)]);
    request.structure.as_mut().unwrap().relationships = vec![call(), call()];
    let result = execute(store, SemanticOperation::PublishRevisions(request)).unwrap();
    let SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(receipt)) =
        result
    else {
        panic!("expected publication receipt")
    };
    receipt
}

fn definitions(
    store: &dyn SemanticPersistence,
    query: &SemanticRevisionConnectionQuery,
) -> Vec<SemanticRevisionConnection> {
    let result = execute(store, SemanticOperation::RevisionConnections(query.clone())).unwrap();
    let SemanticResult::RevisionConnections(edges) = result else {
        panic!("expected exact connection definitions")
    };
    edges
}

fn collect_old_sources(store: &dyn SemanticPersistence) {
    let policy = SemanticRevisionRetentionPolicy::RevisionCount { revisions: 1 };
    let result = execute(
        store,
        SemanticOperation::PlanRevisionRetention {
            project_root: ROOT.into(),
            operation_id: "collect".into(),
            policy: policy.clone(),
        },
    )
    .unwrap();
    let SemanticResult::RevisionRetentionPlan(plan) = result else {
        panic!("expected retention plan")
    };
    let result = execute(
        store,
        SemanticOperation::CollectRevisionRetention { plan, policy },
    )
    .unwrap();
    assert!(
        matches!(result, SemanticResult::RevisionRetention(receipt) if receipt.pruned_records.len() == 2)
    );
}

fn exercise(
    store: &dyn SemanticPersistence,
) -> (
    SemanticRevisionConnectionQuery,
    Vec<SemanticRevisionConnection>,
) {
    let receipt = publish(store, "initial", 1);
    let query = SemanticRevisionConnectionQuery {
        project_root: ROOT.into(),
        project_id: RevisionTestClient::new(store, ROOT, "local").project_id(),
        connection_ids: receipt
            .accepted_scope
            .connection_ids
            .into_iter()
            .rev()
            .collect(),
    };
    let before = definitions(store, &query);
    assert_eq!(before.len(), 2);
    assert_ne!(before[0].connection_id, before[1].connection_id);
    publish(store, "changed", 2);
    collect_old_sources(store);
    assert_eq!(definitions(store, &query), before);
    let mut wrong = query.clone();
    wrong.project_id = "another-lineage".into();
    assert!(execute(store, SemanticOperation::RevisionConnections(wrong)).is_err());
    (query, before)
}

#[test]
fn retained_connections_resolve_through_both_ports_after_endpoint_collection_and_reopen() {
    for remote in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (query, before) = {
            let local = Arc::new(LocalPersistence::open(directory.path().join("storage")).unwrap());
            let store: Arc<dyn SemanticPersistence> = if remote {
                Arc::new(CentralizedPersistence::new(
                    Arc::new(LocalPublicationServer(local)),
                    "checkout",
                ))
            } else {
                local
            };
            exercise(&*store)
        };
        let reopened = LocalPersistence::open(directory.path().join("storage")).unwrap();
        assert_eq!(definitions(&reopened, &query), before);
    }
}

#[test]
fn missing_and_duplicate_requested_connections_never_return_partial_results() {
    let local = LocalPersistence::in_memory().unwrap();
    let receipt = publish(&local, "initial", 1);
    let id = &receipt.accepted_scope.connection_ids[0];
    for ids in [
        vec![id.clone(), "missing".into()],
        vec![id.clone(), id.clone()],
    ] {
        let query = SemanticRevisionConnectionQuery {
            project_root: ROOT.into(),
            project_id: RevisionTestClient::new(&local, ROOT, "local").project_id(),
            connection_ids: ids,
        };
        assert!(execute(&local, SemanticOperation::RevisionConnections(query)).is_err());
    }
}

fn publish_call_presence(
    store: &dyn SemanticPersistence,
    operation_id: &str,
    partitioned: bool,
    present: bool,
) -> SemanticRevisionPublicationReceipt {
    let elements = if partitioned {
        vec![source("a", 1)]
    } else {
        vec![source("a", 1), source("b", 1)]
    };
    let mut request =
        RevisionTestClient::new(store, ROOT, "local").publication(operation_id, elements);
    let structure = request.structure.as_mut().unwrap();
    structure.relationships = if present { vec![call()] } else { vec![] };
    structure.partition = partitioned.then(|| SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec!["a.rs".into()],
        provider_instance_id: None,
    });
    match execute(store, SemanticOperation::PublishRevisions(request)).unwrap() {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt,
        other => panic!("publication {operation_id}: {other:?}; expected accepted source update"),
    }
}

fn selected_call_definitions(
    store: &dyn SemanticPersistence,
) -> PersistenceResult<Vec<SemanticRevisionConnection>> {
    match execute(
        store,
        SemanticOperation::RevisionSelection {
            project_root: ROOT.into(),
            selection_id: "working".into(),
        },
    )? {
        SemanticResult::RevisionSelection(Some(snapshot)) => Ok(snapshot.connections),
        other => panic!("selection {other:?}; expected the current working snapshot"),
    }
}

#[test]
fn retained_deselected_connection_readd_preserves_exact_reads_after_reopen() {
    for remote in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (query, before) = {
            let local = Arc::new(LocalPersistence::open(directory.path().join("storage")).unwrap());
            let store: Arc<dyn SemanticPersistence> = if remote {
                Arc::new(CentralizedPersistence::new(
                    Arc::new(LocalPublicationServer(local)),
                    "checkout",
                ))
            } else {
                local
            };
            let initial = publish_call_presence(&*store, "initial-call", false, true);
            let query = SemanticRevisionConnectionQuery {
                project_root: ROOT.into(),
                project_id: RevisionTestClient::new(&*store, ROOT, "local").project_id(),
                connection_ids: initial.accepted_scope.connection_ids,
            };
            let before = definitions(&*store, &query);
            assert_eq!(before.len(), 1);
            publish_call_presence(&*store, "remove-call", true, false);
            assert!(selected_call_definitions(&*store).unwrap().is_empty());
            assert_eq!(definitions(&*store, &query), before);
            let restored = publish_call_presence(&*store, "readd-call", true, true);
            assert_eq!(restored.accepted_scope.connection_ids, query.connection_ids);
            let selected = selected_call_definitions(&*store);
            assert!(
                selected.is_ok(),
                "remote={remote}: re-added retained edge must remain readable: {selected:?}"
            );
            assert_eq!(selected.unwrap(), before);
            assert_eq!(definitions(&*store, &query), before);
            (query, before)
        };
        let reopened = LocalPersistence::open(directory.path().join("storage")).unwrap();
        assert_eq!(selected_call_definitions(&reopened).unwrap(), before);
        assert_eq!(definitions(&reopened, &query), before);
    }
}

fn publish_child_presence(
    store: &dyn SemanticPersistence,
    operation_id: &str,
    partitioned: bool,
    present: bool,
) -> SemanticRevisionPublicationReceipt {
    let mut elements = if partitioned {
        vec![]
    } else {
        vec![source("p", 1)]
    };
    if present {
        let mut child = source("c", 1);
        child.parent_element_id = Some("p".into());
        elements.push(child);
    }
    let mut request =
        RevisionTestClient::new(store, ROOT, "local").publication(operation_id, elements);
    request.structure.as_mut().unwrap().partition = partitioned.then(|| SemanticPartition {
        project_root: ROOT.into(),
        replace_paths: vec!["c.rs".into()],
        provider_instance_id: None,
    });
    assert!(request.structure.as_ref().unwrap().relationships.is_empty());
    match execute(store, SemanticOperation::PublishRevisions(request)).unwrap() {
        SemanticResult::RevisionPublication(SemanticRevisionPublicationOutcome::Accepted(
            receipt,
        )) => receipt,
        other => panic!("publication {operation_id}: {other:?}; expected accepted child update"),
    }
}

#[test]
fn retained_derived_containment_readd_preserves_exact_reads_after_reopen() {
    for remote in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (query, before) = {
            let local = Arc::new(LocalPersistence::open(directory.path().join("storage")).unwrap());
            let store: Arc<dyn SemanticPersistence> = if remote {
                Arc::new(CentralizedPersistence::new(
                    Arc::new(LocalPublicationServer(local)),
                    "checkout",
                ))
            } else {
                local
            };
            let initial = publish_child_presence(&*store, "initial-child", false, true);
            let query = SemanticRevisionConnectionQuery {
                project_root: ROOT.into(),
                project_id: RevisionTestClient::new(&*store, ROOT, "local").project_id(),
                connection_ids: initial.accepted_scope.connection_ids,
            };
            let before = definitions(&*store, &query);
            assert_eq!(before.len(), 1);
            assert_eq!(before[0].relationship.relationship_kind, "contains");
            publish_child_presence(&*store, "remove-child", true, false);
            assert!(selected_call_definitions(&*store).unwrap().is_empty());
            assert_eq!(definitions(&*store, &query), before);
            let restored = publish_child_presence(&*store, "readd-child", true, true);
            assert_eq!(restored.accepted_scope.connection_ids, query.connection_ids);
            let selected = selected_call_definitions(&*store);
            assert!(
                selected.is_ok(),
                "remote={remote}: re-added derived containment must remain readable: {selected:?}"
            );
            assert_eq!(selected.unwrap(), before);
            assert_eq!(definitions(&*store, &query), before);
            (query, before)
        };
        let reopened = LocalPersistence::open(directory.path().join("storage")).unwrap();
        assert_eq!(selected_call_definitions(&reopened).unwrap(), before);
        assert_eq!(definitions(&reopened, &query), before);
    }
}
