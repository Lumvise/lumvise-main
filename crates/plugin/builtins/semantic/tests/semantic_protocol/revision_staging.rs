//! Frozen observations and typed conflicts cross the signed compiled-plugin protocol.
use super::*;
use serde_json::{Value, json};

fn flat_batch() -> Value {
    json!({"project_root":"/work/revisions", "provider_instance_id":"files",
        "semantic_sources":[{"semantic_source_id":"shared-source", "kind":"filesystem",
            "name":"Files", "root_uri":"file:///work/revisions"}],
        "semantic_elements":[
            {"semantic_source_id":"shared-source", "semantic_element_id":"a", "path":"a.rs",
                "semantic_element_type":"file", "semantic_element_name":"A1"},
            {"semantic_source_id":"shared-source", "semantic_element_id":"b", "path":"b.rs",
                "semantic_element_type":"file", "semantic_element_name":"B1"}],
        "semantic_relationships":[]})
}

fn staged_edit(observation: &Value, id: &str, file: &str, name: &str) -> Value {
    let mut batch = flat_batch();
    let elements = batch["semantic_elements"].as_array_mut().unwrap();
    elements.retain(|element| element["path"] == file);
    elements[0]["semantic_element_name"] = json!(name);
    batch["revision_observation"] = observation.clone();
    batch["replace_paths"] = json!([file]);
    batch["ingestion_job_id"] = json!(id);
    batch["ingestion_page_index"] = json!(0);
    batch["ingestion_page_count"] = json!(1);
    batch
}

#[test]
fn signed_single_batch_requires_its_producer_base_before_any_write() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[120; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    let error = failure(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, flat_batch())
            .unwrap(),
    );
    assert_eq!(error.code, "missing_revision_observation");
    assert_eq!(broker.semantic_counts("/work/revisions"), (0, 0));
    assert_eq!(broker.row_count(PLUGIN_ID, "semantic_sources"), 0);
}

#[test]
fn signed_single_batches_preserve_disjoint_changes_and_reject_stale_overlaps() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[121; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    let observe = || {
        success(
            runtime
                .invoke(
                    PLUGIN_ID,
                    "observe_index_base",
                    json!({
        "project_root":"/work/revisions", "provider_instance_id":"files",
        "paths":[], "artifact_ids":[]}),
                )
                .unwrap(),
        )
    };
    let mut initial = flat_batch();
    initial["revision_observation"] = observe();
    let initialized = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, initial)
            .unwrap(),
    );
    assert_eq!(initialized["publication"]["outcome"], "accepted");
    let observation = observe();
    let single = |file: &str, name: &str| {
        let mut batch = staged_edit(&observation, "unused-page-id", file, name);
        for key in [
            "ingestion_job_id",
            "ingestion_page_index",
            "ingestion_page_count",
        ] {
            batch.as_object_mut().unwrap().remove(key);
        }
        batch
    };
    let a = single("a.rs", "A2");
    let accepted = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, a.clone())
            .unwrap(),
    );
    assert_eq!(accepted["publication"]["outcome"], "accepted");
    assert_eq!(
        success(
            runtime
                .invoke(PLUGIN_ID, INGEST_EXPORT_ID, single("b.rs", "B2"))
                .unwrap()
        )["accepted"],
        true
    );
    assert_eq!(
        success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, a).unwrap()),
        accepted
    );
    let conflict = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, single("a.rs", "A3"))
            .unwrap(),
    );
    assert_eq!(conflict["accepted"], false);
    assert_eq!(conflict["publication"]["outcome"], "conflict");
    let selected = broker.selected_revision_snapshot("/work/revisions");
    assert_eq!(
        selected
            .elements
            .iter()
            .find(|element| element.path == "a.rs")
            .unwrap()
            .name,
        "A2"
    );
    assert_eq!(
        selected
            .elements
            .iter()
            .find(|element| element.path == "b.rs")
            .unwrap()
            .name,
        "B2"
    );
}

#[test]
fn signed_single_batch_failure_preserves_the_complete_previous_graph() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[122; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                broker.observe_fixture(flat_batch()),
            )
            .unwrap(),
    );
    let before = broker.projected_records("/work/revisions");
    let mut invalid = flat_batch();
    invalid["semantic_elements"][0]["semantic_element_name"] = json!("Must not leak");
    invalid["semantic_artifacts"] = json!([{"artifact_id":"broken-payload", "semantic_element_id":"a",
        "artifact_kind":"annotation", "title":"Invalid evidence", "content":"Must not leak",
        "dependencies":[{"target":{"target_kind":"artifact", "artifact_id":"missing-evidence"}}]}]);
    let invalid = broker.observe_fixture(invalid);
    failure(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, invalid)
            .unwrap(),
    );
    assert_eq!(broker.projected_records("/work/revisions"), before);
    assert_eq!(broker.semantic_operation_count("sync_partition"), 0);
    assert_eq!(broker.semantic_operation_count("upsert_artifact"), 0);
    assert_eq!(broker.semantic_operation_count("remove_artifact"), 0);
}

#[test]
fn signed_replay_preserves_the_newer_source_descriptor() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[123; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                broker.observe_fixture(flat_batch()),
            )
            .unwrap(),
    );
    let observation = broker.revision_observation("/work/revisions", "files");
    let mut a = staged_edit(&observation, "source-descriptor-a", "a.rs", "A2");
    a["semantic_sources"][0]["name"] = json!("First accepted descriptor");
    let first = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, a.clone())
            .unwrap(),
    );
    let mut b = staged_edit(&observation, "source-descriptor-b", "b.rs", "B2");
    b["semantic_sources"][0]["name"] = json!("Newer accepted descriptor");
    success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, b).unwrap());
    assert_eq!(
        broker.source_name("/work/revisions", "shared-source"),
        "Newer accepted descriptor"
    );
    assert_eq!(
        success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, a).unwrap()),
        first
    );
    assert_eq!(
        broker.source_name("/work/revisions", "shared-source"),
        "Newer accepted descriptor"
    );
}

#[test]
fn cooperating_hosts_fence_source_projection_and_adopt_the_original_graph_receipt() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[124; 32]));
    let first_broker = Arc::new(MemoryCapabilityBroker::default());
    let second_broker = Arc::new(first_broker.cooperating_instance());
    let first = Arc::new(system(first_broker.clone()));
    let second = system(second_broker.clone());
    for runtime in [first.as_ref(), &second] {
        runtime.install(&installed).unwrap();
        runtime.start(PLUGIN_ID).unwrap();
    }
    success(
        first
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                first_broker.observe_fixture(flat_batch()),
            )
            .unwrap(),
    );
    let observation = first_broker.revision_observation("/work/revisions", "files");
    let mut a = staged_edit(&observation, "source-race-a", "a.rs", "A2");
    a["semantic_sources"][0]["name"] = json!("Older paused descriptor");
    let mut b = staged_edit(&observation, "source-race-b", "b.rs", "B2");
    b["semantic_sources"][0]["name"] = json!("Newer cooperating descriptor");
    let gate = first_broker.pause_next_source_commit();
    let (pending_runtime, pending_input) = (first.clone(), a.clone());
    let pending = std::thread::spawn(move || {
        pending_runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, pending_input)
    });
    gate.wait();
    let newer = success(second.invoke(PLUGIN_ID, INGEST_EXPORT_ID, b).unwrap());
    gate.release();
    let adopted = success(pending.join().unwrap().unwrap());
    let accepted_graph = first_broker.revision_history("/work/revisions");
    let replayed = success(first.invoke(PLUGIN_ID, INGEST_EXPORT_ID, a).unwrap());
    assert_eq!(adopted["publication"], replayed["publication"]);
    assert_eq!(
        replayed["publication"]["value"]["operation_id"],
        "source-race-a"
    );
    assert!(
        replayed["publication"]["value"]["commit_version"]
            .as_i64()
            .unwrap()
            < newer["publication"]["value"]["commit_version"]
                .as_i64()
                .unwrap()
    );
    assert_eq!(
        first_broker.revision_history("/work/revisions"),
        accepted_graph
    );
    for broker in [first_broker, second_broker] {
        assert_eq!(
            broker.source_name("/work/revisions", "shared-source"),
            "Newer cooperating descriptor"
        );
        let selected = broker.selected_revision_snapshot("/work/revisions");
        assert_eq!(
            selected
                .elements
                .iter()
                .find(|element| element.path == "a.rs")
                .unwrap()
                .name,
            "A2"
        );
        assert_eq!(
            selected
                .elements
                .iter()
                .find(|element| element.path == "b.rs")
                .unwrap()
                .name,
            "B2"
        );
    }
    first.stop(PLUGIN_ID).unwrap();
    second.stop(PLUGIN_ID).unwrap();
}

#[test]
fn signed_indexing_retains_the_original_observation_and_returns_conflicts() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[117; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                broker.observe_fixture(flat_batch()),
            )
            .unwrap(),
    );
    let observation = broker.revision_observation("/work/revisions", "files");
    let mut producer_observation = observation.clone();
    producer_observation["producer_base"] =
        json!({"selection_id":"producer:compiled-checkout", "expected_generation":0});
    let mut a = staged_edit(&producer_observation, "edit-a", "a.rs", "A2");
    a["semantic_elements"][0]["revision_dependencies"] = json!([{
        "kind":"element", "record_id":"b"
    }]);
    let accepted = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, a.clone())
            .unwrap(),
    );
    assert_eq!(accepted["publication"]["outcome"], "accepted");
    assert_eq!(
        accepted["publication"]["value"]["producer_base"]["selection_id"],
        "producer:compiled-checkout"
    );
    assert_eq!(
        accepted["publication"]["value"]["producer_base"]["generation"],
        1
    );
    let b = staged_edit(&observation, "edit-b", "b.rs", "B2");
    assert_eq!(
        success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, b).unwrap())["accepted"],
        true
    );
    let elements = broker
        .selected_revision_snapshot("/work/revisions")
        .elements;
    assert_eq!(
        elements
            .iter()
            .find(|element| element.path == "a.rs")
            .unwrap()
            .revision_dependencies,
        [lumvise_db_core::SemanticRecordRef::element("b")]
    );
    assert_ne!(
        elements
            .iter()
            .find(|element| element.path == "b.rs")
            .unwrap()
            .semantic_element_id,
        "b"
    );
    assert_eq!(
        success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, a).unwrap()),
        accepted
    );
    let stale = staged_edit(&observation, "stale-a", "a.rs", "A3");
    let conflict = success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, stale).unwrap());
    assert_eq!(conflict["accepted"], false);
    assert_eq!(conflict["ingestion_status"], "failed");
    assert_eq!(conflict["publication"]["outcome"], "conflict");
}

#[test]
fn signed_indexing_requires_a_frozen_base_before_the_first_page() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[118; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    let mut batch = staged_edit(&Value::Null, "unobserved", "a.rs", "A2");
    batch
        .as_object_mut()
        .unwrap()
        .remove("revision_observation");
    let error = failure(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch).unwrap());
    assert_eq!(error.code, "missing_revision_observation");
    assert_eq!(broker.semantic_counts("/work/revisions"), (0, 0));
}

fn document_pages(observation: &Value) -> [Value; 2] {
    let mut first = staged_edit(observation, "document-body", "a.rs", "Document");
    first["ingestion_page_count"] = json!(2);
    first["semantic_elements"][0]["revision_dependencies"] =
        json!([{"kind":"artifact", "record_id":"payload"}]);
    first["semantic_artifacts"] = json!([{"artifact_id":"payload", "semantic_element_id":"a",
        "artifact_kind":"knowledge_document", "title":"Document", "content":"Frozen body", "metadata":{},
        "dependencies":[{"target":{"target_kind":"semantic_element", "semantic_element_id":"b"}},
            {"target":{"target_kind":"artifact", "artifact_id":"resource"}}]},
        {"artifact_id":"resource", "semantic_element_id":"b", "artifact_kind":"document_assets",
            "title":"Resources", "metadata":{}}
    ]);
    let mut last = first.clone();
    last["ingestion_page_index"] = json!(1);
    last["semantic_elements"] = json!([]);
    last["semantic_artifacts"] = json!([]);
    [first, last]
}

fn staged_body_reference(observation: &Value, first: &Value) -> String {
    let write = serde_json::from_value::<lumvise_db_core::SemanticArtifactRevisionWrite>(json!({
        "artifact":first["semantic_artifacts"][0], "media_type":"text/plain"
    }))
    .unwrap();
    write
        .stored_artifact(observation["project_id"].as_str().unwrap())
        .unwrap()
        .content_ref
        .unwrap()
}

#[test]
fn signed_staging_publishes_locator_payload_and_exact_evidence_in_one_commit() {
    let workspace = tempfile::tempdir().unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[119; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                broker.observe_fixture(flat_batch()),
            )
            .unwrap(),
    );
    let observation = broker.revision_observation("/work/revisions", "files");
    let [first, last] = document_pages(&observation);
    let reference = staged_body_reference(&observation, &first);
    let staged = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, first.clone())
            .unwrap(),
    );
    assert_eq!(staged["ingestion_status"], "running");
    assert!(broker.revision_blob(&reference).is_none());
    assert!(
        broker
            .selected_revision_snapshot("/work/revisions")
            .artifacts
            .is_empty()
    );
    let accepted = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, last.clone())
            .unwrap(),
    );
    assert_eq!(accepted["publication"]["outcome"], "accepted");
    assert_document_payload(&broker, &reference);
    success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, first).unwrap());
    assert_eq!(
        success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, last).unwrap()),
        accepted
    );
    runtime.stop(PLUGIN_ID).unwrap();
}

fn assert_document_payload(broker: &MemoryCapabilityBroker, reference: &str) {
    let selected = broker.selected_revision_snapshot("/work/revisions");
    let locator = selected.elements.iter().find(|e| e.path == "a.rs").unwrap();
    let payload = selected
        .artifacts
        .iter()
        .find(|a| a.artifact_id == "payload")
        .unwrap();
    assert_eq!(
        locator.revision_dependencies,
        [lumvise_db_core::SemanticRecordRef::artifact("payload")]
    );
    assert_eq!(payload.semantic_element_id, locator.semantic_element_id);
    assert_eq!(payload.content.as_deref(), Some("Frozen body"));
    assert_eq!(payload.dependencies[0].target_id(), "b");
    assert_eq!(payload.dependencies[1].target_id(), "resource");
    assert!(
        selected
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == "resource")
    );
    let blob = broker.revision_blob(reference).unwrap();
    assert_eq!(blob.media_type, "text/plain");
    assert_eq!(blob.content, b"Frozen body");
}
