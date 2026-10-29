use lumvise_db_core::{
    LocalPersistence, ProjectSnapshotScope, RelationalOperation, RelationalPersistence,
    RelationalResult, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use std::sync::Arc;

mod revision_publication;
mod revision_staging;

fn observed_revision(db: &LocalPersistence) -> Value {
    let SemanticResult::ProjectIdentity(identity) = semantic_result(
        db,
        SemanticOperation::ProjectIdentity {
            project_root: "/repo".into(),
        },
    ) else {
        panic!("expected lineage");
    };
    let SemanticResult::RevisionObservation(head) = semantic_result(
        db,
        SemanticOperation::RevisionSelectionMembers(
            lumvise_db_core::SemanticRevisionSelectionQuery {
                project_root: "/repo".into(),
                selection_id: "working".into(),
                structure_coverage: lumvise_db_core::SemanticRevisionStructureCoverage::Project,
                artifact_ids: vec![],
            },
        ),
    ) else {
        panic!("expected observed revision members");
    };
    json!(lumvise_contracts::SemanticIndexObservation {
        project_id: identity.project_id,
        producer_base: None,
        selection_id: "working".into(),
        source_context: "local".into(),
        expected: head.map_or_else(Default::default, |head| head.expected),
    })
}

fn broker_for(db: Arc<LocalPersistence>) -> AppCoreHostCapabilityBroker {
    let semantic: Arc<dyn SemanticPersistence> = db.clone();
    let relational: Arc<dyn RelationalPersistence> = db;
    AppCoreHostCapabilityBroker::new(semantic, relational)
}

fn semantic_result(db: &LocalPersistence, operation: SemanticOperation) -> SemanticResult {
    <LocalPersistence as SemanticPersistence>::execute(
        db,
        operation,
        &InvocationControl::sixty_seconds(),
    )
    .expect("semantic persistence operation")
}

fn relational_result(db: &LocalPersistence, operation: RelationalOperation) -> RelationalResult {
    <LocalPersistence as RelationalPersistence>::execute(
        db,
        operation,
        &InvocationControl::sixty_seconds(),
    )
    .expect("relational persistence operation")
}

fn active_project_elements(db: &LocalPersistence) -> Vec<lumvise_db_core::SemanticElement> {
    match semantic_result(
        db,
        SemanticOperation::ProjectSnapshot {
            scope: ProjectSnapshotScope::ProjectRoot("/repo".into()),
            artifact_namespace: None,
        },
    ) {
        SemanticResult::ProjectSnapshot(snapshot) => snapshot
            .elements
            .into_iter()
            .filter(|element| element.lifecycle == "active")
            .collect(),
        result => panic!("unexpected project snapshot result: {result:?}"),
    }
}
fn artifact(db: &LocalPersistence, artifact_id: &str) -> lumvise_db_core::SemanticArtifact {
    match semantic_result(
        db,
        SemanticOperation::Artifact {
            artifact_id: artifact_id.into(),
        },
    ) {
        SemanticResult::Artifact(Some(artifact)) => artifact,
        result => panic!("unexpected artifact result: {result:?}"),
    }
}

fn artifact_blob_len(db: &LocalPersistence, content_ref: &str) -> usize {
    match semantic_result(
        db,
        SemanticOperation::ArtifactBlobGet {
            content_ref: content_ref.into(),
        },
    ) {
        SemanticResult::ArtifactBlob(Some(blob)) => blob.content.len(),
        result => panic!("unexpected artifact blob result: {result:?}"),
    }
}

fn plugin_data_is_empty(db: &LocalPersistence, plugin_id: &str, table_name: &str) -> bool {
    matches!(
        relational_result(
            db,
            RelationalOperation::ListPluginData {
                plugin_id: plugin_id.into(),
                table_name: table_name.into(),
            },
        ),
        RelationalResult::PluginDataRows(rows) if rows.is_empty()
    )
}

fn persistence() -> Arc<LocalPersistence> {
    Arc::new(LocalPersistence::in_memory().expect("local persistence"))
}

use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};

use super::*;
use crate::AppCoreHostCapabilityBroker;

#[test]
fn unrelated_plugin_cannot_write_semantic_structure() {
    let broker = broker_for(persistence());
    let error = broker.invoke(request(
        "third.party",
        json!({"operation": "sync_structure", "project_root": "/repo",
            "elements": [], "relationships": []}),
    ));
    assert!(error.is_err());
}

#[test]
fn controlled_attachment_copy_returns_staged_bytes_before_destination_publication() {
    let db = persistence();
    let broker = broker_for(db.clone());
    broker.invoke(request(SEMANTIC_PLUGIN_ID, json!({"operation":"sync_structure", "project_root":"/repo", "elements":[element()], "relationships":[]}))).unwrap();
    broker.invoke(request(KNOWLEDGE_PLUGIN_ID, json!({"operation":"upsert_artifact", "artifact":{
        "artifact_id":"source", "semantic_element_id":"element-1", "artifact_kind":"media", "title":"Source",
        "content":"source", "content_ref":null, "metadata":{}}}))).unwrap();
    semantic_result(
        &db,
        SemanticOperation::ArtifactBlobPut {
            content_ref: "canvas-file:source:image".into(),
            artifact_id: "source".into(),
            media_type: "image/png".into(),
            content: vec![1, 2, 3],
        },
    );
    let context = lumvise_plugin_runtime::PluginInvocationContext::new(
        "copy",
        "human",
        lumvise_plugin_runtime::PluginInvocationClass::Foreground,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    let copied = broker.invoke_controlled(request(KNOWLEDGE_PLUGIN_ID, json!({"operation":"copy_artifact_attachment", "source_artifact_id":"source", "target_artifact_id":"destination", "content_ref":"canvas-file:source:image"})), &context).unwrap();
    assert_eq!(
        artifact_blob_len(&db, copied["content_ref"].as_str().unwrap()),
        3
    );
}

#[test]
fn assistant_may_read_project_roots_without_semantic_write_access() {
    let broker = broker_for(persistence());
    let roots = broker
        .invoke(request(
            "builtin.assistant",
            json!({"operation":"project_roots"}),
        ))
        .expect("Assistant project membership read");
    assert_eq!(roots["project_roots"], json!([]));
    assert!(
        broker
            .invoke(request(
                "builtin.assistant",
                json!({"operation":"sync_structure",
        "project_root":"/repo", "elements":[], "relationships":[]})
            ))
            .is_err()
    );
}

#[test]
fn semantic_plugin_stores_and_searches_native_element_vectors() {
    let broker = broker_for(persistence());
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "sync_structure", "project_root": "/repo",
                "elements": [element_for("/repo", "near"), element_for("/repo", "far")],
                "relationships": []}),
        ))
        .unwrap();
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation": "store_element_name_vectors",
                "project_root": "/repo",
                "vectors": [
                    {"semantic_element_id": "near", "project_root": "/repo",
                        "source_text": "lib.rs",
                        "vector": {"engine_id": "neural.embed", "model": null,
                            "dimensions": 2, "vector": [1.0, 0.0],
                            "normalized": false, "metadata": {}}},
                    {"semantic_element_id": "far", "project_root": "/repo",
                        "source_text": "lib.rs",
                        "vector": {"engine_id": "neural.embed", "model": null,
                            "dimensions": 2, "vector": [0.0, 1.0],
                            "normalized": false, "metadata": {}}}
                ]
            }),
        ))
        .unwrap();

    let output = broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "search_element_name_vectors", "project_root": "/repo",
                "query": [1.0, 0.0], "k": 1, "engine_id": "neural.embed"}),
        ))
        .unwrap();
    assert_eq!(output["results"].as_array().unwrap().len(), 1);
    assert_eq!(output["results"][0]["id"], "near");
}

#[test]
fn staged_project_snapshot_is_invisible_until_atomic_commit() {
    let db = persistence();
    let broker = broker_for(Arc::clone(&db));
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "sync_structure", "project_root": "/repo",
                "elements": [element_for("/repo", "old")], "relationships": []}),
        ))
        .unwrap();
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "begin_project_snapshot", "snapshot_id": "job-1",
                "project_root": "/repo", "partition_paths": [], "page_count": 2,
                "observation": observed_revision(db.as_ref())}),
        ))
        .unwrap();
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation": "stage_project_snapshot", "snapshot_id": "job-1", "page_index": 0,
                "elements": [element_for("/repo", "first")], "relationships": []
            }),
        ))
        .unwrap();

    let before = active_project_elements(db.as_ref());
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].semantic_element_id, "old");

    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation": "stage_project_snapshot", "snapshot_id": "job-1", "page_index": 1,
                "elements": [element_for("/repo", "second")], "relationships": []
            }),
        ))
        .unwrap();
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation": "commit_project_snapshot", "snapshot_id": "job-1"
            }),
        ))
        .unwrap();

    let output = broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "elements_by_ids", "project_root": "/repo",
                "semantic_element_ids": ["second", "missing"]}),
        ))
        .unwrap();
    let after = active_project_elements(db.as_ref());
    assert_eq!(
        after
            .iter()
            .filter(|element| element.lifecycle == "active")
            .count(),
        2
    );
    assert!(
        after
            .iter()
            .all(|element| element.semantic_element_id != "old")
    );
    assert_eq!(output["elements"].as_array().unwrap().len(), 1);
    assert_eq!(output["elements"][0]["semantic_element_id"], "second");
}

#[test]
fn staged_project_snapshot_rejects_out_of_order_pages() {
    let db = persistence();
    let broker = broker_for(Arc::clone(&db));
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation": "begin_project_snapshot", "snapshot_id": "job-order",
                "project_root": "/repo", "partition_paths": [], "page_count": 2,
                "observation": observed_revision(db.as_ref())
            }),
        ))
        .unwrap();

    let error = broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({
                "operation": "stage_project_snapshot", "snapshot_id": "job-order", "page_index": 1,
                "elements": [], "relationships": []
            }),
        ))
        .unwrap_err();

    assert!(format!("{error:?}").contains("semantic_snapshot_page_out_of_order"));
}

#[test]
fn project_snapshot_returns_one_complete_protobuf_value() {
    let db = persistence();
    let broker = broker_for(Arc::clone(&db));
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "sync_structure", "project_root": "/repo",
                "elements": [element_for("/repo", "first"), element_for("/repo", "second")],
                "relationships": [{"project_root": "/repo", "source_element_id": "first",
                    "target_element_id": "second", "relationship_kind": "calls",
                    "label": "calls", "metadata": {}}]}),
        ))
        .unwrap();

    let snapshot = broker
        .invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "project_snapshot", "scope": {"project_root": "/repo"},
                "artifact_namespace": "knowledge"}),
        ))
        .unwrap();

    assert_eq!(snapshot["elements"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["relationships"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["relationships"][0]["lifecycle"], "active");
    assert!(snapshot.get("snapshot_id").is_none());
    assert!(snapshot.get("next_offset").is_none());
}

#[test]
fn large_knowledge_content_spills_to_sql_and_keeps_graph_node() {
    let db = persistence();
    let broker = broker_for(Arc::clone(&db));
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "sync_structure", "project_root": "/repo",
                "elements": [element()], "relationships": []}),
        ))
        .unwrap();
    let content = "x".repeat(8 * 1024 + 1);
    broker
        .invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "upsert_artifact", "artifact": {
                "artifact_id": "knowledge-1", "semantic_element_id": "element-1",
                "artifact_kind": "decision", "title": "Decision",
                "content_ref": null, "content": content,
                "searchable_text": null, "content_size_bytes": null,
                "metadata": {"knowledge": {}}
            }}),
        ))
        .unwrap();
    let artifact = artifact(db.as_ref(), "knowledge-1");
    assert_eq!(artifact.content.as_deref(), Some(content.as_str()));
    let content_ref = artifact.content_ref.unwrap();
    assert_eq!(artifact_blob_len(db.as_ref(), &content_ref), 8 * 1024 + 1);
    let snapshot = broker
        .invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "project_snapshot", "scope": {"project_root": "/repo"},
                "artifact_namespace": "knowledge"}),
        ))
        .unwrap();
    assert_eq!(snapshot["artifacts"][0]["content"], content);
    let attached = broker
        .invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "artifacts_for_elements",
                "semantic_element_ids": ["element-1"]}),
        ))
        .unwrap();
    assert_eq!(attached["artifacts"][0]["content"], content);
    assert!(plugin_data_is_empty(
        db.as_ref(),
        SEMANTIC_PLUGIN_ID,
        "semantic_elements",
    ));
    assert!(plugin_data_is_empty(
        db.as_ref(),
        KNOWLEDGE_PLUGIN_ID,
        "knowledge_artifacts",
    ));
}

#[test]
fn project_artifact_read_excludes_artifacts_owned_by_other_projects() {
    let db = persistence();
    let broker = broker_for(Arc::clone(&db));
    for (project_root, element_id) in [("/repo-a", "element-a"), ("/repo-b", "element-b")] {
        broker
            .invoke(request(
                SEMANTIC_PLUGIN_ID,
                json!({"operation": "sync_structure", "project_root": project_root,
                    "elements": [element_for(project_root, element_id)], "relationships": []}),
            ))
            .unwrap();
        broker
            .invoke(request(
                KNOWLEDGE_PLUGIN_ID,
                json!({"operation": "upsert_artifact", "artifact": {
                    "artifact_id": format!("artifact-{element_id}"),
                    "semantic_element_id": element_id, "artifact_kind": "definition",
                    "title": element_id, "content_ref": null, "content": "content",
                    "searchable_text": null, "content_size_bytes": null,
                    "metadata": {"knowledge": {}}
                }}),
            ))
            .unwrap();
    }
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "upsert_artifact", "artifact": {
                "artifact_id": "source-artifact", "semantic_element_id": "element-a",
                "artifact_kind": "source", "title": "Source", "content_ref": null,
                "content": "source", "searchable_text": null,
                "content_size_bytes": null, "metadata": {"source": {}}
            }}),
        ))
        .unwrap();

    let output = broker
        .invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "project_snapshot", "scope": {"project_root": "/repo-a"},
                "artifact_namespace": "knowledge"}),
        ))
        .unwrap();

    assert_eq!(output["artifacts"].as_array().unwrap().len(), 1);
}

#[test]
fn candidate_source_elements_is_authorized_for_knowledge_only() {
    let broker = broker_for(persistence());
    let denied = broker.invoke(request(
        SEMANTIC_PLUGIN_ID,
        json!({"operation": "candidate_source_elements", "content_fingerprints": [],
            "kind_name_keys": [], "kind_file_name_keys": []}),
    ));
    assert!(denied.is_err());
    let allowed = broker.invoke(request(
        KNOWLEDGE_PLUGIN_ID,
        json!({"operation": "candidate_source_elements", "content_fingerprints": [],
            "kind_name_keys": [], "kind_file_name_keys": []}),
    ));
    assert!(allowed.is_ok());
}

#[test]
fn candidate_source_elements_unions_identity_key_matches_across_projects() {
    let db = persistence();
    let broker = broker_for(Arc::clone(&db));
    let mut fingerprint_source = element_for("/repo-a", "fp-source");
    fingerprint_source["content_fingerprint"] = json!("fp1:0000000000000001:same-hash");
    let mut fingerprint_twin = element_for("/repo-b", "fp-twin");
    fingerprint_twin["content_fingerprint"] = json!("fp1:0000000000000001:same-hash");
    let mut kind_name_twin = element_for("/repo-b", "kind-name-twin");
    kind_name_twin["name"] = json!("Shared");
    kind_name_twin["element_kind"] = json!("function");
    let mut kind_file_twin = element_for("/repo-b", "kind-file-twin");
    kind_file_twin["path"] = json!("other/shared.rs");
    kind_file_twin["element_kind"] = json!("file");
    let unrelated = element_for("/repo-b", "unrelated");
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "sync_structure", "project_root": "/repo-a",
                "elements": [fingerprint_source], "relationships": []}),
        ))
        .unwrap();
    broker
        .invoke(request(
            SEMANTIC_PLUGIN_ID,
            json!({"operation": "sync_structure", "project_root": "/repo-b",
                "elements": [fingerprint_twin, kind_name_twin, kind_file_twin, unrelated],
                "relationships": []}),
        ))
        .unwrap();

    let output = broker
        .invoke(request(
            KNOWLEDGE_PLUGIN_ID,
            json!({"operation": "candidate_source_elements",
                "content_fingerprints": ["fp1:0000000000000001:same-hash"],
                "kind_name_keys": ["function\u{1}shared"],
                "kind_file_name_keys": ["file\u{1}shared.rs"]}),
        ))
        .unwrap();
    let mut ids = output["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|element| element["semantic_element_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec!["fp-source", "fp-twin", "kind-file-twin", "kind-name-twin"]
    );
}

fn element() -> Value {
    element_for("/repo", "element-1")
}

fn element_for(project_root: &str, semantic_element_id: &str) -> Value {
    json!({
        "project_root": project_root, "semantic_element_id": semantic_element_id,
        "semantic_source_id": "source-1", "path": "src/lib.rs",
        "element_kind": "file", "name": "lib.rs", "parent_element_id": null,
        "content_fingerprint": null, "start_line": null, "end_line": null,
        "lifecycle": "active", "match_evidence": null, "metadata": {}
    })
}

fn request(plugin_id: &str, input: Value) -> HostCapabilityRequest {
    HostCapabilityRequest {
        plugin_id: plugin_id.into(),
        invocation_id: "invocation".into(),
        call_id: "call".into(),
        capability_id: SEMANTIC_STORAGE.into(),
        required_version: "^1.0".into(),
        input,
    }
}

#[test]
fn project_and_batched_artifact_reads_are_limited_to_semantic_and_knowledge() {
    let broker = broker_for(persistence());
    for (plugin_id, allowed) in [
        (SEMANTIC_PLUGIN_ID, true),
        (KNOWLEDGE_PLUGIN_ID, true),
        (NUCLEUS_PLUGIN_ID, false),
        ("builtin.assistant", false),
        ("third.party", false),
    ] {
        for input in [
            json!({"operation": "project_artifacts", "project_root": "/repo", "artifact_namespace": null}),
            json!({"operation": "artifacts_by_ids", "artifact_ids": ["missing"]}),
        ] {
            let result = broker.invoke(request(plugin_id, input));
            assert_eq!(result.is_ok(), allowed, "{plugin_id}: {result:?}");
        }
    }
}
