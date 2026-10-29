//! The real host capability preserves frozen observations through independent staged writers.
use super::*;

fn source(id: &str, name: &str) -> Value {
    let mut element = element_for("/repo", id);
    element["path"] = json!(format!("src/{id}.rs"));
    element["name"] = json!(name);
    element
}

fn begin(broker: &AppCoreHostCapabilityBroker, id: &str, path: &str, observation: &Value) {
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation":"begin_project_snapshot", "snapshot_id":id, "project_root":"/repo",
                "partition_paths":[path], "page_count":1, "observation": observation,
            }),
        ))
        .unwrap();
}

fn stage(
    broker: &AppCoreHostCapabilityBroker,
    id: &str,
    element: Value,
    relationships: Vec<Value>,
) {
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation":"stage_project_snapshot", "snapshot_id":id, "page_index":0,
                "elements":[element], "relationships":relationships,
            }),
        ))
        .unwrap();
}

fn commit(broker: &AppCoreHostCapabilityBroker, id: &str) -> Value {
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation":"commit_project_snapshot", "snapshot_id":id}),
        ))
        .unwrap()
}

fn seeded() -> Arc<LocalPersistence> {
    let db = persistence();
    broker_for(Arc::clone(&db))
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation":"sync_structure", "project_root":"/repo",
                "elements":[source("a","A1"),source("b","B1")], "relationships":[],
            }),
        ))
        .unwrap();
    db
}

#[test]
fn independent_staged_disjoint_edits_accept_and_overlapping_observations_conflict() {
    let db = seeded();
    let observation = observed_revision(db.as_ref());
    let first = broker_for(Arc::clone(&db));
    let second = broker_for(Arc::clone(&db));
    begin(&first, "edit-a", "src/a.rs", &observation);
    begin(&second, "edit-b", "src/b.rs", &observation);
    stage(&first, "edit-a", source("a", "A2"), vec![]);
    stage(&second, "edit-b", source("b", "B2"), vec![]);
    assert_eq!(commit(&first, "edit-a")["accepted"], true);
    assert_eq!(commit(&second, "edit-b")["accepted"], true);
    begin(&second, "stale-a", "src/a.rs", &observation);
    stage(&second, "stale-a", source("a", "A3"), vec![]);
    let conflict = commit(&second, "stale-a");
    assert_eq!(conflict["accepted"], false);
    assert_eq!(conflict["publication"]["outcome"], "conflict");
    let names = active_project_elements(db.as_ref())
        .into_iter()
        .map(|element| element.name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names, ["A2".into(), "B2".into()].into_iter().collect());
}

fn duplicate_calls() -> Vec<Value> {
    let edge = json!({"project_root":"/repo","source_element_id":"a", "target_element_id":"b",
        "relationship_kind":"calls", "label":"calls", "metadata":{}});
    vec![edge.clone(), edge]
}

#[test]
fn restaging_a_frozen_operation_after_broker_restart_replays_duplicate_connection_ids() {
    let db = seeded();
    let observation = observed_revision(db.as_ref());
    let publish = |broker: &AppCoreHostCapabilityBroker| {
        begin(broker, "lost-ack", "src/a.rs", &observation);
        stage(broker, "lost-ack", source("a", "A2"), duplicate_calls());
        commit(broker, "lost-ack")
    };
    let receipt = publish(&broker_for(Arc::clone(&db)));
    let ids = receipt["publication"]["value"]["accepted_scope"]["connection_ids"]
        .as_array()
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(publish(&broker_for(Arc::clone(&db))), receipt);
}

#[test]
fn an_unobserved_snapshot_cannot_begin() {
    let broker = broker_for(persistence());
    assert!(broker.invoke(request(SEMANTIC_PLUGIN_ID, json!({
        "operation":"begin_project_snapshot", "snapshot_id":"missing-base", "project_root":"/repo",
        "partition_paths":[], "page_count":1,
    }))).is_err());
}

fn document_payload(id: &str) -> lumvise_db_core::SemanticArtifactRevisionWrite {
    serde_json::from_value(json!({"artifact":{
        "artifact_id":id, "semantic_element_id":"b", "artifact_kind":"knowledge_document",
        "title":"Document body", "content":"Exact frozen contents", "content_ref":null,
        "searchable_text":null, "content_size_bytes":null,
        "dependencies":[{"target":{"target_kind":"semantic_element", "semantic_element_id":"b"}}],
        "metadata":{}
    }, "media_type":"text/plain"}))
    .unwrap()
}

fn stage_document_payload(broker: &AppCoreHostCapabilityBroker, operation: &str, id: &str) {
    let mut locator = source("a", "Document locator");
    locator["revision_dependencies"] = json!([{"kind":"artifact","record_id":id}]);
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation":"stage_project_snapshot", "snapshot_id":operation, "page_index":0,
                "elements":[locator], "relationships":[], "artifacts":[document_payload(id)]
            }),
        ))
        .unwrap();
}

fn assert_no_payload(db: &LocalPersistence, id: &str, observation: &Value) {
    let reference = document_payload(id)
        .stored_artifact(observation["project_id"].as_str().unwrap())
        .unwrap()
        .content_ref
        .unwrap();
    assert!(matches!(
        semantic_result(
            db,
            SemanticOperation::Artifact {
                artifact_id: id.into()
            }
        ),
        SemanticResult::Artifact(None)
    ));
    assert!(matches!(
        semantic_result(
            db,
            SemanticOperation::ArtifactBlobGet {
                content_ref: reference
            }
        ),
        SemanticResult::ArtifactBlob(None)
    ));
}

#[test]
fn staged_document_payloads_commit_atomically_and_replay_after_broker_restart() {
    let db = seeded();
    let observation = observed_revision(&db);
    let broker = broker_for(db.clone());
    begin(&broker, "document", "src/a.rs", &observation);
    stage_document_payload(&broker, "document", "payload");
    assert_no_payload(&db, "payload", &observation);
    assert_eq!(
        active_project_elements(&db)
            .iter()
            .find(|e| e.path == "src/a.rs")
            .unwrap()
            .name,
        "A1"
    );
    let receipt = commit(&broker, "document");
    assert_eq!(receipt["accepted"], true);
    assert_eq!(
        artifact(&db, "payload").content.as_deref(),
        Some("Exact frozen contents")
    );
    let restarted = broker_for(db.clone());
    begin(&restarted, "document", "src/a.rs", &observation);
    stage_document_payload(&restarted, "document", "payload");
    assert_eq!(commit(&restarted, "document"), receipt);
    remove_primary_payload(&restarted, &db, observation);
}

fn remove_primary_payload(
    broker: &AppCoreHostCapabilityBroker,
    db: &LocalPersistence,
    mut observation: Value,
) {
    let SemanticResult::RevisionSelection(Some(selected)) = semantic_result(
        db,
        SemanticOperation::RevisionSelection {
            project_root: "/repo".into(),
            selection_id: "working".into(),
        },
    ) else {
        panic!("expected the selected document publication");
    };
    observation["expected"] = json!(selected.expectation);
    let locator = selected
        .elements
        .iter()
        .find(|e| e.path == "src/a.rs")
        .unwrap();
    begin(broker, "detach-payload", "src/a.rs", &observation);
    broker.invoke(request(SEMANTIC_PLUGIN_ID, json!({
        "operation":"stage_project_snapshot", "snapshot_id":"detach-payload", "page_index":0,
        "elements":[locator], "relationships":[], "removed_artifact_ids":["payload"]
    }))).unwrap();
    assert_eq!(commit(broker, "detach-payload")["accepted"], true);
    assert_eq!(
        artifact(db, "payload").content.as_deref(),
        Some("Exact frozen contents")
    );
}

#[test]
fn a_conflicted_staged_document_leaves_no_payload_or_blob() {
    let db = seeded();
    let observation = observed_revision(&db);
    let broker = broker_for(db.clone());
    begin(&broker, "winner", "src/a.rs", &observation);
    stage(&broker, "winner", source("a", "A2"), vec![]);
    assert_eq!(commit(&broker, "winner")["accepted"], true);
    begin(&broker, "stale-document", "src/a.rs", &observation);
    stage_document_payload(&broker, "stale-document", "unaccepted-payload");
    assert_eq!(commit(&broker, "stale-document")["accepted"], false);
    assert_no_payload(&db, "unaccepted-payload", &observation);
    assert_eq!(
        active_project_elements(&db)
            .iter()
            .find(|e| e.path == "src/a.rs")
            .unwrap()
            .name,
        "A2"
    );
}
