use super::*;

#[test]
fn storage_trigger_lazily_indexes_changed_element_and_artifact() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[71; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-source", "semantic_element_id": "fn:parse",
        "artifact_kind": "source", "title": "Parse source",
        "searchable_text": "parse input tree"
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest"),
    );

    // Before any trigger fires, no vectors exist yet - search falls back to lexical.
    let before = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({
                    "project_root": "/work/demo", "query": "parse semantic graph", "limit": 5
                }),
            )
            .expect("search before trigger"),
    );
    assert_eq!(before["mode"], "lexical_fallback");
    assert!(broker.element_name_vector_engine("fn:parse").is_none());
    assert!(broker.artifact_text_vector_engine("parse-source").is_none());

    *broker.embed_available.lock().expect("embed switch") = true;
    let element_change = serde_json::json!({
        "project_root": "/work/demo",
        "base_revision": 0,
        "target_revision": 1,
        "changed": [{
            "entity_id": "fn:parse", "entity_kind": "semantic_element",
            "disposition": "upserted"
        }]
    });
    let element_response = success(
        runtime
            .invoke(PLUGIN_ID, ELEMENT_TRIGGER_EXPORT_ID, element_change)
            .expect("element trigger"),
    );
    assert_eq!(element_response["acknowledged"], true);
    assert_eq!(
        broker.element_name_vector_engine("fn:parse").as_deref(),
        Some("test-fixture-embed")
    );

    let artifact_change = serde_json::json!({
        "project_root": "/work/demo",
        "base_revision": 1,
        "target_revision": 2,
        "changed": [{
            "entity_id": "parse-source", "entity_kind": "semantic_artifact",
            "disposition": "upserted"
        }]
    });
    let artifact_response = success(
        runtime
            .invoke(PLUGIN_ID, ARTIFACT_TRIGGER_EXPORT_ID, artifact_change)
            .expect("artifact trigger"),
    );
    assert_eq!(artifact_response["acknowledged"], true);
    assert_eq!(
        broker
            .artifact_text_vector_engine("parse-source")
            .as_deref(),
        Some("test-fixture-embed")
    );

    // Now that the element's vector is stored, search resolves through the vector path
    // and finds the exact element - no full rebuild was ever invoked.
    let after = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({
                    "project_root": "/work/demo", "query": "parse semantic graph", "limit": 5
                }),
            )
            .expect("search after trigger"),
    );
    assert_eq!(after["mode"], "vector");
    assert_eq!(
        after["results"][0]["element"]["semantic_element_id"],
        "fn:parse"
    );

    // A trigger for an element that no longer exists is a harmless no-op.
    let stale_change = serde_json::json!({
        "project_root": "/work/demo",
        "base_revision": 2,
        "target_revision": 3,
        "changed": [{
            "entity_id": "fn:missing", "entity_kind": "semantic_element",
            "disposition": "upserted"
        }]
    });
    let stale_response = success(
        runtime
            .invoke(PLUGIN_ID, ELEMENT_TRIGGER_EXPORT_ID, stale_change)
            .expect("stale element trigger"),
    );
    assert_eq!(stale_response["acknowledged"], true);
    assert_eq!(stale_response["processed"], 0);
}

#[test]
fn storage_trigger_batches_duplicate_mixed_changes_with_bounded_calls() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[72; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-source", "semantic_element_id": "fn:parse",
        "artifact_kind": "source", "title": "Parse source",
        "searchable_text": "parse input tree"
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest"),
    );
    *broker.embed_available.lock().expect("embed switch") = true;
    broker.reset_semantic_operation_counts();
    let response = success(
        runtime.invoke(
            PLUGIN_ID,
            ELEMENT_TRIGGER_EXPORT_ID,
            serde_json::json!({
                "project_root": "/work/demo", "base_revision": 0, "target_revision": 1,
                "changed": [
                    {"entity_id": "fn:parse", "entity_kind": "semantic_element", "disposition": "upserted"},
                    {"entity_id": "fn:parse", "entity_kind": "semantic_element", "disposition": "upserted"},
                    {"entity_id": "missing", "entity_kind": "semantic_element", "disposition": "upserted"},
                    {"entity_id": "fn:parse", "entity_kind": "semantic_element", "disposition": "removal"},
                    {"entity_id": "parse-source", "entity_kind": "semantic_artifact", "disposition": "upserted"},
                    {"entity_id": "parse-source", "entity_kind": "semantic_artifact", "disposition": "upserted"},
                    {"entity_id": "missing-artifact", "entity_kind": "semantic_artifact", "disposition": "upserted"},
                    {"entity_id": "parse-source", "entity_kind": "semantic_artifact", "disposition": "removal"}
                ]
            }),
        ).expect("batched trigger"),
    );
    assert_eq!(response["acknowledged"], true);
    assert_eq!(response["processed"], 2);
    assert_eq!(broker.semantic_operation_count("elements_by_ids"), 1);
    assert_eq!(broker.semantic_operation_count("artifacts_by_ids"), 1);
    assert_eq!(
        broker.semantic_operation_count("store_element_name_vectors"),
        1
    );
    assert_eq!(
        broker.semantic_operation_count("store_artifact_text_vectors"),
        1
    );
    let sizes = broker.embed_batch_sizes();
    assert_eq!(sizes[sizes.len() - 2..], [1, 1]);
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn storage_trigger_batch_failure_is_not_acknowledged() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[73; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
            .expect("ingest"),
    );
    *broker.embed_available.lock().expect("embed switch") = true;
    broker.fail_next_commit();
    let error = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                ELEMENT_TRIGGER_EXPORT_ID,
                serde_json::json!({
                    "project_root": "/work/demo", "base_revision": 0, "target_revision": 1,
                    "changed": [{"entity_id": "fn:parse", "entity_kind": "semantic_element",
                        "disposition": "upserted"}]
                }),
            )
            .expect("failed trigger wire"),
    );
    assert_eq!(error.code, "injected_commit_failure");
    runtime.stop(PLUGIN_ID).expect("stop");
}
