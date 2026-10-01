#[path = "semantic_protocol/code_intelligence.rs"]
mod code_intelligence;
#[path = "semantic_protocol/context_scope.rs"]
mod context_scope;
#[path = "semantic_protocol/duplicate_identities.rs"]
mod duplicate_identities;
#[path = "semantic_protocol/snapshot_project.rs"]
mod snapshot_project;
mod support;

use ed25519_dalek::SigningKey;
use lumvise_plugin_package::{ExecutionMode, HostCompatibility, verify_package};
use lumvise_plugin_protocol::WireOutcome;
use lumvise_plugin_semantic::{
    ARTIFACT_TRIGGER_EXPORT_ID, DEPENDENCY_TREE_EXPORT_ID, ELEMENT_AT_LOCATION_EXPORT_ID,
    ELEMENT_TRIGGER_EXPORT_ID, GRAPH_PROVIDERS_EXPORT_ID, INGEST_EXPORT_ID,
    LATEST_INDEX_LOG_EXPORT_ID, MANIFEST_EXPORT_ID, PACKAGE_PROTOCOL_VERSION, PLUGIN_ID,
    REBUILD_SEARCH_INDEX_EXPORT_ID, RECORD_INDEX_LOG_EXPORT_ID, SEARCH_EXPORT_ID,
    SEMANTIC_CONTEXT_EXPORT_ID, SEMANTIC_GRAPH_EXPORT_ID, SNAPSHOT_STATUS_EXPORT_ID,
    TREE_EXPORT_ID, package_manifest_source,
};
use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::Arc,
};

use support::{ControlledTestInvoke, MemoryCapabilityBroker, signed_install, system};

fn success(outcome: WireOutcome) -> serde_json::Value {
    match outcome {
        WireOutcome::Succeeded { value } => value,
        other => panic!("expected success, got {other:?}"),
    }
}

fn failure(outcome: WireOutcome) -> lumvise_plugin_protocol::PluginWireError {
    match outcome {
        WireOutcome::Failed { error } => error,
        other => panic!("expected failure, got {other:?}"),
    }
}

fn index_batch() -> serde_json::Value {
    serde_json::json!({
        "provider_instance_id": "rust-indexer",
        "project_root": "/work/demo",
        "semantic_sources": [{
            "semantic_source_id": "source-main", "kind": "repository", "name": "demo",
            "root_path": "/work/demo", "root_uri": "file:///work/demo"
        }],
        "semantic_elements": [
            {"semantic_source_id": "source-main", "semantic_element_id": "file:parser",
             "path": "src/parser.rs", "semantic_element_type": "file",
             "semantic_element_name": "parser.rs", "start_line": 1, "end_line": 80},
            {"semantic_source_id": "source-main", "semantic_element_id": "fn:parse",
             "path": "src/parser.rs", "semantic_element_type": "function",
             "semantic_element_name": "parse semantic graph", "start_line": 10, "end_line": 30},
            {"semantic_source_id": "source-main", "semantic_element_id": "fn:render",
             "path": "src/render.rs", "semantic_element_type": "function",
             "semantic_element_name": "render graph", "start_line": 4, "end_line": 18}
        ],
        "semantic_relationships": [
            {"source_element_id": "file:parser", "target_element_id": "fn:parse",
             "relationship_kind": "contains", "relationship_label": "contains"},
            {"source_element_id": "fn:parse", "target_element_id": "fn:render",
             "relationship_kind": "calls", "relationship_label": "calls"},
            {"source_element_id": "fn:render", "target_element_id": "fn:parse",
             "relationship_kind": "calls", "relationship_label": "calls"}
        ]
    })
}

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

#[test]
fn ingest_explicitly_removes_semantic_artifacts_and_publishes_deletion() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[70; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut seeded = index_batch();
    seeded["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-note", "semantic_element_id": "fn:parse",
        "artifact_kind": "annotation", "title": "Parse note", "content": "Remove me"
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, seeded)
            .expect("seed artifact"),
    );
    let mut removal = index_batch();
    removal["removed_artifact_ids"] = serde_json::json!(["parse-note"]);

    let removed = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, removal.clone())
            .expect("remove artifact"),
    );
    assert_eq!(removed["semantic_artifacts_removed"], 1);
    let repeated = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, removal)
            .expect("repeat removal"),
    );
    assert_eq!(repeated["semantic_artifacts_removed"], 0);
}

#[test]
fn paged_ingest_publishes_only_after_complete_appcore_snapshot() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[61; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut first = index_batch();
    first["ingestion_job_id"] = serde_json::json!("atomic-job");
    first["ingestion_page_index"] = serde_json::json!(0);
    first["ingestion_page_count"] = serde_json::json!(2);
    first["semantic_elements"] = serde_json::json!([
        first["semantic_elements"][0].clone(),
        first["semantic_elements"][1].clone()
    ]);
    first["semantic_relationships"] = serde_json::json!([]);

    let staged = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, first)
            .expect("stage"),
    );
    assert_eq!(staged["ingestion_status"], "running");
    assert_eq!(broker.semantic_counts("/work/demo"), (0, 0));

    let mut final_page = index_batch();
    final_page["ingestion_job_id"] = serde_json::json!("atomic-job");
    final_page["ingestion_page_index"] = serde_json::json!(1);
    final_page["ingestion_page_count"] = serde_json::json!(2);
    final_page["semantic_sources"] = serde_json::json!([]);
    final_page["semantic_elements"] =
        serde_json::json!([final_page["semantic_elements"][2].clone()]);

    let completed = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, final_page)
            .expect("commit"),
    );
    assert_eq!(completed["ingestion_status"], "completed");
    assert_eq!(broker.semantic_counts("/work/demo"), (3, 3));
}

#[test]
fn file_graph_includes_semantic_children_and_their_first_neighbors() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[63; 32]));
    let runtime = system(Arc::new(MemoryCapabilityBroker::default()));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"]
        .as_array_mut()
        .expect("semantic elements")
        .push(serde_json::json!({
            "semantic_source_id": "source-main", "semantic_element_id": "fn:leaf",
            "path": "src/leaf.rs", "semantic_element_type": "function",
            "semantic_element_name": "second hop"
        }));
    batch["semantic_relationships"]
        .as_array_mut()
        .expect("semantic relationships")
        .push(serde_json::json!({
            "source_element_id": "fn:render", "target_element_id": "fn:leaf",
            "relationship_kind": "semantic", "relationship_label": "calls"
        }));
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest"),
    );

    let graph = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_GRAPH_EXPORT_ID,
                serde_json::json!({
                    "projectRoot": "/work/demo",
                    "targetPath": "src/parser.rs",
                    "granularity": "property",
                    "includeFirstNeighbors": true
                }),
            )
            .expect("file graph with first neighbors"),
    );
    let node_ids = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|node| node["id"].as_str().expect("node id"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(node_ids, ["file:parser", "fn:parse", "fn:render"].into());
    assert_eq!(graph["edges"].as_array().expect("edges").len(), 3);
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn signed_plugin_persists_partitioned_graph_and_restores_after_restart() {
    let workspace = tempfile::tempdir().expect("workspace");
    let signing_key = SigningKey::from_bytes(&[41; 32]);
    let (_, installed) = signed_install(workspace.path(), &signing_key);
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");

    let ingested = success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
            .expect("ingest"),
    );
    assert_eq!(ingested["semantic_elements_upserted"], 3);
    assert_eq!(broker.semantic_counts("/work/demo"), (3, 3));
    runtime.stop(PLUGIN_ID).expect("stop");
    runtime.start(PLUGIN_ID).expect("restart");
    let tree = success(
        runtime
            .invoke(
                PLUGIN_ID,
                TREE_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo"}),
            )
            .expect("tree"),
    );
    assert_eq!(tree["total_nodes"], 3);
    assert_eq!(
        tree["roots"][0]["element"]["semantic_element_id"],
        "file:parser"
    );
    broker.reset_unprefixed_element_lists();
    broker.reset_project_snapshot_calls();
    let subtree = success(
        runtime
            .invoke(
                PLUGIN_ID,
                TREE_EXPORT_ID,
                serde_json::json!({"semantic_element_id": "file:parser"}),
            )
            .expect("element subtree"),
    );
    assert_eq!(subtree["total_nodes"], 2);
    assert_eq!(broker.project_snapshot_calls(), 0);

    let cycle = success(
        runtime
            .invoke(
                PLUGIN_ID,
                DEPENDENCY_TREE_EXPORT_ID,
                serde_json::json!({"semantic_element_id": "fn:parse", "direction": "both"}),
            )
            .expect("dependency tree"),
    );
    assert_eq!(
        cycle["root"]["dependencies"][0]["node"]["element"]["semantic_element_id"],
        "fn:render"
    );
    assert_eq!(
        cycle["root"]["dependencies"][0]["node"]["dependencies"][0]["cycle"],
        true
    );
    assert_eq!(cycle["total_nodes"], 3);
    assert_eq!(broker.project_snapshot_calls(), 0);
    assert_eq!(broker.unprefixed_element_lists(), 0);
    runtime.stop(PLUGIN_ID).expect("final stop");
}

#[test]
fn rich_index_parent_hints_project_function_calls_to_file_edges() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[61; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let batch = serde_json::json!({
        "provider_instance_id": "lumvise-mcp-project-indexer",
        "project_root": "/work/rich",
        "semantic_sources": [{
            "semantic_source_id": "source-main", "kind": "repository", "name": "rich",
            "root_path": "/work/rich", "root_uri": "file:///work/rich"
        }],
        "semantic_elements": [
            {"semantic_source_id": "source-main", "semantic_element_id": "file:caller",
             "path": "src/caller.rs", "semantic_element_type": "file",
             "semantic_element_name": "caller.rs"},
            {"semantic_source_id": "source-main", "semantic_element_id": "fn:caller",
             "parent_element_id": "file:caller", "path": "src/caller.rs",
             "semantic_element_type": "function", "semantic_element_name": "caller"},
            {"semantic_source_id": "source-main", "semantic_element_id": "file:callee",
             "path": "src/callee.rs", "semantic_element_type": "file",
             "semantic_element_name": "callee.rs"},
            {"semantic_source_id": "source-main", "semantic_element_id": "fn:callee",
             "parent_element_id": "file:callee", "path": "src/callee.rs",
             "semantic_element_type": "function", "semantic_element_name": "callee"}
        ],
        "semantic_relationships": [{
            "source_element_id": "fn:caller", "target_element_id": "fn:callee",
            "relationship_kind": "semantic", "relationship_label": "calls"
        }]
    });
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest rich batch"),
    );

    let graph = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_GRAPH_EXPORT_ID,
                serde_json::json!({"projectRoot": "/work/rich", "granularity": "file"}),
            )
            .expect("file graph"),
    );
    assert_eq!(graph["edges"].as_array().expect("edges").len(), 1);
    assert_eq!(graph["edges"][0]["source"], "file:caller");
    assert_eq!(graph["edges"][0]["target"], "file:callee");
    assert_eq!(graph["edges"][0]["linkType"], "calls");
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn relationship_labels_distinguish_edges_with_shared_kind_and_endpoints() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[62; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let batch = serde_json::json!({
        "provider_instance_id": "lumvise-mcp-project-indexer",
        "project_root": "/work/edge-identity",
        "semantic_sources": [],
        "semantic_elements": [
            {"semantic_source_id": "source-main", "semantic_element_id": "file:left",
             "path": "src/left.rs", "semantic_element_type": "file",
             "semantic_element_name": "left.rs"},
            {"semantic_source_id": "source-main", "semantic_element_id": "file:right",
             "path": "src/right.rs", "semantic_element_type": "file",
             "semantic_element_name": "right.rs"}
        ],
        "semantic_relationships": [
            {"source_element_id": "file:left", "target_element_id": "file:right",
             "relationship_kind": "semantic", "relationship_label": "calls"},
            {"source_element_id": "file:left", "target_element_id": "file:right",
             "relationship_kind": "semantic", "relationship_label": "uses_type"}
        ]
    });
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest labeled edges"),
    );

    let graph = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_GRAPH_EXPORT_ID,
                serde_json::json!({
                    "projectRoot": "/work/edge-identity", "granularity": "file"
                }),
            )
            .expect("graph labeled edges"),
    );
    let link_types = graph["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|edge| edge["linkType"].as_str().expect("link type"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        link_types,
        std::collections::BTreeSet::from(["calls", "uses_type"])
    );
    assert_eq!(broker.semantic_counts("/work/edge-identity").1, 2);
    let dependencies = success(
        runtime
            .invoke(
                PLUGIN_ID,
                DEPENDENCY_TREE_EXPORT_ID,
                serde_json::json!({
                    "semantic_element_id": "file:left", "include_descendants": false
                }),
            )
            .expect("labeled dependencies"),
    );
    assert_eq!(
        dependencies["root"]["dependencies"]
            .as_array()
            .expect("dependencies")
            .len(),
        2
    );
    for branch in dependencies["root"]["dependencies"].as_array().unwrap() {
        assert_eq!(branch["node"]["element"]["name"], "right.rs");
        assert_eq!(branch["node"]["element"]["element_kind"], "file");
        assert_eq!(branch["node"]["element"]["path"], "src/right.rs");
    }
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn failed_atomic_replacement_keeps_previous_committed_graph_visible() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[46; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
            .expect("initial ingest"),
    );
    broker.fail_next_commit();
    let replacement = serde_json::json!({
        "provider_instance_id": "rust-indexer", "project_root": "/work/demo",
        "replace_paths": [], "semantic_sources": [],
        "semantic_elements": [{"semantic_source_id": "source-main",
            "semantic_element_id": "file:new", "path": "src/new.rs",
            "semantic_element_type": "file", "semantic_element_name": "new.rs"}],
        "semantic_relationships": []
    });
    let outcome = runtime
        .invoke(PLUGIN_ID, INGEST_EXPORT_ID, replacement)
        .expect("failed outcome");
    assert!(matches!(outcome, WireOutcome::Failed { .. }));
    let tree = success(
        runtime
            .invoke(
                PLUGIN_ID,
                TREE_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo"}),
            )
            .expect("tree after failure"),
    );
    assert_eq!(tree["total_nodes"], 3);
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn staged_ingest_commits_more_than_one_mutation_page() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[47; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let elements = (0..260)
        .map(|index| {
            serde_json::json!({
                "semantic_source_id": "source-main",
                "semantic_element_id": format!("fn:{index}"),
                "path": format!("src/{index}.rs"),
                "semantic_element_type": "function",
                "semantic_element_name": format!("function {index}")
            })
        })
        .collect::<Vec<_>>();
    let batch = serde_json::json!({
        "provider_instance_id": "rust-indexer", "project_root": "/work/large",
        "semantic_sources": [{"semantic_source_id": "source-main", "kind": "repository",
            "name": "large", "root_path": "/work/large", "root_uri": "file:///work/large"}],
        "semantic_elements": elements, "semantic_relationships": []
    });
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("large ingest"),
    );
    assert_eq!(broker.semantic_counts("/work/large").0, 260);
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn rebuild_search_index_batches_260_embeddings_without_reordering_vectors() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[48; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let elements = (0..260)
        .map(|index| {
            serde_json::json!({
                "semantic_source_id": "source-main",
                "semantic_element_id": format!("fn:{index}"),
                "path": format!("src/{index}.rs"),
                "semantic_element_type": "function",
                "semantic_element_name": format!("batch-element-{index}")
            })
        })
        .collect::<Vec<_>>();
    let batch = serde_json::json!({
        "provider_instance_id": "rust-indexer", "project_root": "/work/batched",
        "semantic_sources": [{"semantic_source_id": "source-main", "kind": "repository",
            "name": "batched", "root_path": "/work/batched", "root_uri": "file:///work/batched"}],
        "semantic_elements": elements, "semantic_relationships": []
    });
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("large ingest"),
    );

    *broker.embed_available.lock().expect("embed switch") = true;
    broker.reset_project_snapshot_calls();
    let rebuild = success(
        runtime
            .invoke(
                PLUGIN_ID,
                REBUILD_SEARCH_INDEX_EXPORT_ID,
                serde_json::json!({"project_root": "/work/batched"}),
            )
            .expect("complete rebuild"),
    );

    assert_eq!(rebuild["index_state"], "ready");
    assert_eq!(rebuild["indexed_nodes"], 260);
    assert!(!rebuild.as_object().unwrap().contains_key("continuation"));
    assert_eq!(broker.embed_batch_sizes(), vec![128, 128, 4]);
    assert_eq!(broker.project_snapshot_calls(), 1);
    let vectors = broker
        .element_name_vectors("/work/batched")
        .into_iter()
        .map(|vector| (vector.semantic_element_id.clone(), vector))
        .collect::<HashMap<_, _>>();
    assert_eq!(vectors.len(), 260);
    for index in 0..260 {
        let vector = vectors
            .get(&format!("fn:{index}"))
            .expect("stored vector for semantic element");
        assert_eq!(vector.source_text, format!("batch-element-{index}"));
        assert_eq!(vector.vector.vector, vec![index as f32, 1.0, 0.0, 0.0]);
    }
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn search_never_backfills_vectors_and_reports_index_readiness() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[42; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
            .expect("ingest"),
    );
    *broker.embed_available.lock().expect("embed switch") = true;
    let missing = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"query": "parser", "project_root": "/work/demo"}),
            )
            .expect("bounded search"),
    );
    assert_eq!(missing["mode"], "lexical_fallback");
    assert_eq!(missing["index_state"], "missing");
    assert_eq!(
        missing["results"][0]["element"]["semantic_element_id"],
        "file:parser"
    );
    assert_eq!(broker.row_count(PLUGIN_ID, "semantic_element_vectors"), 0);

    let rebuild = success(
        runtime
            .invoke(
                PLUGIN_ID,
                REBUILD_SEARCH_INDEX_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo"}),
            )
            .expect("complete rebuild"),
    );
    assert_eq!(rebuild["index_state"], "ready");
    assert_eq!(rebuild["indexed_nodes"], 3);
    assert!(!rebuild.as_object().unwrap().contains_key("continuation"));
    assert_eq!(broker.row_count(PLUGIN_ID, "semantic_element_vectors"), 0);
    let vector = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"query": "parser", "project_root": "/work/demo"}),
            )
            .expect("vector search after rebuild"),
    );
    assert_eq!(vector["mode"], "vector");
    assert_eq!(vector["index_state"], "ready");

    *broker.embed_available.lock().expect("embed switch") = false;
    let lexical = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"query": "render", "project_root": "/work/demo"}),
            )
            .expect("lexical search"),
    );
    assert_eq!(lexical["mode"], "lexical_fallback");
    assert_eq!(
        lexical["results"][0]["element"]["semantic_element_id"],
        "fn:render"
    );
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn graph_exports_and_invalid_input_are_public_contracts() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[43; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(broker.clone());
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
            .expect("ingest"),
    );

    let providers = success(
        runtime
            .invoke(PLUGIN_ID, GRAPH_PROVIDERS_EXPORT_ID, serde_json::json!({}))
            .expect("providers"),
    );
    assert_eq!(providers["providers"][0]["projectRoot"], "/work/demo");
    assert_eq!(broker.unprefixed_element_lists(), 0);
    broker.reset_renderer_graph_calls();
    let graph = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_GRAPH_EXPORT_ID,
                serde_json::json!({"projectRoot": "/work/demo", "granularity": "function"}),
            )
            .expect("graph"),
    );
    assert_eq!(graph["source"], "app-owned-index");
    assert_eq!(graph["nodes"].as_array().expect("nodes").len(), 3);
    assert_eq!(broker.renderer_graph_calls(), 1);
    assert_eq!(broker.project_snapshot_calls(), 0);
    let error = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                SEARCH_EXPORT_ID,
                serde_json::json!({"query": ""}),
            )
            .expect("invalid outcome"),
    );
    assert_eq!(error.code, "invalid_semantic_input");
    runtime.stop(PLUGIN_ID).expect("stop");
}

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
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
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
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, replacement)
            .expect("replace path"),
    );
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
            .any(|result| result["element"]["semantic_element_id"] == "file:parser")
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
    assert_eq!(graph["edges"].as_array().expect("edges").len(), 3);
    let render = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["id"] == "fn:render")
        .expect("retained render node");
    assert_eq!(render["semanticArtifactCount"], 1);
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn manifest_declares_all_exports_and_versioned_host_capabilities() {
    let manifest = package_manifest_source("test-target", &"a".repeat(64));
    assert_eq!(manifest.exports.len(), 30);
    for http_id in [
        "http_semantic_context",
        "http_search_context",
        "http_semantic_relationship_tree",
    ] {
        assert!(
            manifest.exports.iter().any(|e| e.id == http_id),
            "missing HTTP export {http_id}"
        );
    }
    assert_eq!(
        manifest
            .exports
            .iter()
            .find(|export| export.id == REBUILD_SEARCH_INDEX_EXPORT_ID)
            .expect("rebuild export")
            .execution,
        ExecutionMode::Background
    );
    assert!(
        manifest
            .host_capabilities
            .iter()
            .any(|capability| capability.id == "project.source")
    );
    assert!(
        manifest
            .host_capabilities
            .iter()
            .any(|capability| capability.id == "storage.plugin")
    );
    assert!(
        manifest
            .host_capabilities
            .iter()
            .any(|capability| capability.id == "storage.semantic")
    );
    let snapshot_capability = manifest
        .host_capabilities
        .iter()
        .find(|capability| capability.id == "semantic.snapshot")
        .expect("semantic.snapshot capability");
    assert_eq!(snapshot_capability.version, "^1.0");
}

#[test]
fn snapshot_status_uses_compiled_plugin_wire_round_trip() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[51; 32]));
    let runtime = system(Arc::new(MemoryCapabilityBroker::default()));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let outcome = runtime
        .invoke(
            PLUGIN_ID,
            SNAPSHOT_STATUS_EXPORT_ID,
            serde_json::json!({"operation_id": "missing-snapshot-operation"}),
        )
        .expect("controlled plugin invocation");
    match outcome {
        WireOutcome::Failed { error } => {
            assert!(error.message.contains("known semantic snapshot operation"));
        }
        other => panic!("expected failed protobuf result, got {other:?}"),
    }
}
#[test]
fn manifest_invocation_reports_every_packaged_export() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[50; 32]));
    let expected = package_manifest_source("test-target", &"a".repeat(64))
        .exports
        .into_iter()
        .map(|export| export.id)
        .collect::<Vec<_>>();
    let runtime = system(Arc::new(MemoryCapabilityBroker::default()));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");

    let metadata = success(
        runtime
            .invoke(PLUGIN_ID, MANIFEST_EXPORT_ID, serde_json::json!({}))
            .expect("invoke manifest"),
    );

    assert_eq!(metadata["exports"], serde_json::json!(expected));
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn semantic_context_returns_complete_snapshot_with_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[49; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"][1]["content_fingerprint"] =
        serde_json::json!("fp1:0000000000000001:parse");
    batch["semantic_elements"][1]["metadata"] = serde_json::json!({
        "signature": "fn parse(input: &str) -> Result<Tree>"
    });
    batch["semantic_relationships"][1]["target_locator"] = serde_json::json!("crate::render");
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-source", "semantic_element_id": "fn:parse",
        "artifact_kind": "source", "title": "Parse source",
        "content": "fn parse(input: &str) -> Result<Tree>",
        "searchable_text": "parse input tree", "content_size_bytes": 42,
        "metadata": {"language": "rust"}
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest context fixture"),
    );

    broker.reset_project_snapshot_calls();
    let elements = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "record_kind": "elements"}),
            )
            .expect("complete element snapshot"),
    );
    assert_eq!(elements["record_kind"], "elements");
    assert_eq!(elements["elements"].as_array().map(Vec::len), Some(3));
    assert!(elements["commit_version"].as_i64().unwrap_or_default() > 0);
    assert!(elements["published_at"].is_string());
    assert!(!elements.as_object().unwrap().contains_key("next_after_key"));

    let relationships = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "record_kind": "relationships"}),
            )
            .expect("complete relationship snapshot"),
    );
    let relationship_records = relationships["relationships"]
        .as_array()
        .expect("relationships");
    assert_eq!(relationship_records.len(), 3);
    assert!(
        relationship_records
            .iter()
            .any(|item| item["metadata"]["target_locator"] == "crate::render")
    );
    assert!(
        !relationships
            .as_object()
            .unwrap()
            .contains_key("next_after_key")
    );

    let artifacts = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "record_kind": "artifacts"}),
            )
            .expect("complete artifact snapshot"),
    );
    assert_eq!(artifacts["artifacts"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        artifacts["artifacts"][0]["content"],
        "fn parse(input: &str) -> Result<Tree>"
    );
    assert_eq!(broker.unprefixed_element_lists(), 0);
    assert_eq!(broker.full_context_scans(), 0);
    assert_eq!(broker.project_snapshot_calls(), 3);
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn modified_signed_manifest_is_rejected_before_execution() {
    const TARGET: &str = "semantic-integration-host";
    let workspace = tempfile::tempdir().expect("workspace");
    let signing_key = SigningKey::from_bytes(&[44; 32]);
    let (archive, _) = signed_install(workspace.path(), &signing_key);
    let tampered = workspace.path().join("tampered.lvp");
    let reader = std::fs::File::open(&archive).expect("archive");
    let mut source = zip::ZipArchive::new(reader).expect("source zip");
    let output = std::fs::File::create(&tampered).expect("tampered archive");
    let mut writer = zip::ZipWriter::new(output);
    for index in 0..source.len() {
        let mut entry = source.by_index(index).expect("zip entry");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("entry bytes");
        if entry.name() == "manifest.json" {
            let position = bytes
                .windows("builtin.semantic".len())
                .position(|window| window == b"builtin.semantic")
                .expect("plugin id in manifest");
            bytes[position] = b'x';
        }
        writer
            .start_file(entry.name(), zip::write::SimpleFileOptions::default())
            .expect("start copied entry");
        writer.write_all(&bytes).expect("copy entry");
    }
    writer.finish().expect("finish tampered archive");
    assert!(
        verify_package(
            &tampered,
            &signing_key.verifying_key(),
            &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
        )
        .is_err()
    );
}
