//! Exact revision identities and retained endpoints across partial source updates.
use super::*;

#[test]
fn location_logs_and_partition_replacement_preserve_public_semantics() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[45; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                broker.observe_fixture(index_batch()),
            )
            .expect("ingest"),
    );

    broker.reset_project_snapshot_calls();
    broker.reset_semantic_operation_counts();
    let location = success(runtime.invoke(PLUGIN_ID, ELEMENT_AT_LOCATION_EXPORT_ID,
        serde_json::json!({"project_root": "/work/demo", "path": "./src/parser.rs", "line": 12}))
        .expect("location"));
    assert_eq!(location["semantic_element_id"], "fn:parse");
    let absolute_location = success(
        runtime
            .invoke(
                PLUGIN_ID,
                ELEMENT_AT_LOCATION_EXPORT_ID,
                serde_json::json!({
                    "project_root": "/work/demo",
                    "path": "/work/demo/src/parser.rs",
                    "line": 12
                }),
            )
            .expect("absolute location"),
    );
    assert_eq!(
        absolute_location["semantic_element_id"],
        location["semantic_element_id"]
    );
    assert_eq!(absolute_location["path"], "src/parser.rs");

    let outside_location = success(
        runtime
            .invoke(
                PLUGIN_ID,
                ELEMENT_AT_LOCATION_EXPORT_ID,
                serde_json::json!({
                    "project_root": "/work/demo",
                    "path": "/outside/src/parser.rs",
                    "line": 12
                }),
            )
            .expect("outside-root location"),
    );
    assert!(outside_location["semantic_element_id"].is_null());
    assert_eq!(outside_location["candidates"], serde_json::json!([]));
    assert_eq!(broker.project_snapshot_calls(), 0);
    assert_eq!(broker.semantic_operation_count("scoped_read"), 3);

    success(
        runtime
            .invoke(
                PLUGIN_ID,
                RECORD_INDEX_LOG_EXPORT_ID,
                serde_json::json!({
                    "provider_instance_id": "rust-indexer", "project_root": "/work/demo",
                    "index_log_id": "run-1", "status": "completed", "metrics": {"elements": 3}
                }),
            )
            .expect("record log"),
    );
    let latest = success(
        runtime
            .invoke(
                PLUGIN_ID,
                LATEST_INDEX_LOG_EXPORT_ID,
                serde_json::json!({"provider_instance_id": "rust-indexer"}),
            )
            .expect("latest log"),
    );
    assert_eq!(latest["index_log"]["status"], "completed");

    let replacement = serde_json::json!({
        "provider_instance_id": "rust-indexer", "project_root": "/work/demo",
        "replace_paths": ["src/parser.rs"],
        "semantic_sources": [{"semantic_source_id": "source-main", "kind": "repository",
            "name": "demo", "root_path": "/work/demo", "root_uri": "file:///work/demo"}],
        "semantic_elements": [
            {"semantic_source_id": "source-main", "semantic_element_id": "file:parser",
             "path": "src/parser.rs", "semantic_element_type": "file",
             "semantic_element_name": "parser.rs", "start_line": 1, "end_line": 40},
            {"semantic_source_id": "source-main", "semantic_element_id": "fn:parse",
             "parent_element_id": "file:parser", "path": "src/parser.rs",
             "semantic_element_type": "function", "semantic_element_name": "parse",
             "start_line": 10, "end_line": 30}
        ],
        "semantic_relationships": [{
            "source_element_id": "file:parser", "target_element_id": "fn:render",
            "relationship_kind": "calls", "relationship_label": "calls"
        }],
        "semantic_artifacts": [{
            "artifact_id": "render-note", "semantic_element_id": "fn:render",
            "artifact_kind": "annotation", "title": "Cross-partition artifact",
            "content": "retained target"
        }]
    });
    let published = success(
        runtime
            .invoke(
                PLUGIN_ID,
                INGEST_EXPORT_ID,
                broker.observe_fixture(replacement),
            )
            .expect("replace path"),
    );
    let parser_revision = accepted_element_id(&published, "file:parser");
    assert_eq!(broker.semantic_counts("/work/demo").0, 3);
    let search = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"query": "parse", "project_root": "/work/demo"}),
            )
            .expect("search"),
    );
    assert_eq!(search["results"].as_array().expect("results").len(), 2);
    assert!(
        search["results"]
            .as_array()
            .expect("results")
            .iter()
            .any(|result| result["element"]["semantic_element_id"] == parser_revision)
    );
    let graph = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_GRAPH_EXPORT_ID,
                serde_json::json!({"projectRoot": "/work/demo", "granularity": "function"}),
            )
            .expect("graph after partial replacement"),
    );
    assert_eq!(graph["edges"].as_array().expect("edges").len(), 2);
    let selected = broker.selected_revision_snapshot("/work/demo");
    let primary_ids = selected
        .elements
        .iter()
        .map(|element| element.semantic_element_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(!primary_ids.contains("fn:parse"));
    assert!(selected.required_records.iter().any(|stored|matches!(&stored.body,
        lumvise_db_core::SemanticRevisionBody::Element(element) if element.semantic_element_id == "fn:parse")),
        "the current connection must still serve its exact older endpoint");
    let served_ids = primary_ids
        .into_iter()
        .chain(
            selected
                .required_records
                .iter()
                .filter(|stored| stored.record.kind == lumvise_db_core::SemanticRecordKind::Element)
                .map(|stored| stored.record.record_id.as_str()),
        )
        .collect::<std::collections::HashSet<_>>();
    assert!(
        selected.connections.iter().all(|connection| {
            served_ids.contains(connection.relationship.source_element_id.as_str())
                && served_ids.contains(connection.relationship.target_element_id.as_str())
        }),
        "every selected connection must serve both exact endpoints"
    );
    let history = broker.revision_history("/work/demo");
    assert!(
        history
            .connections
            .iter()
            .any(
                |connection| connection.relationship.source_element_id == "fn:render"
                    && connection.relationship.target_element_id == "fn:parse"
            ),
        "historical connection definitions must remain reconstructible"
    );
    let render = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["id"] == "fn:render")
        .expect("retained render node");
    assert_eq!(render["semanticArtifactCount"], 1);
    runtime.stop(PLUGIN_ID).expect("stop");
}
