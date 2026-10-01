use super::*;
use std::collections::HashSet;

#[test]
fn context_scope_honors_root_descendants_and_record_kind() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[73; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"][1]["metadata"] = serde_json::json!({"anchor_selector":{"kind":"pdf_text","page":2,"exact":"Calibration passage"}});
    // Containment relationships and parent links both define descendant scope.
    batch["semantic_elements"][2]["parent_element_id"] = serde_json::json!("fn:parse");
    batch["semantic_elements"].as_array_mut().unwrap().push(serde_json::json!({
        "semantic_source_id": "source-main", "semantic_element_id": "fn:outside",
        "path": "src/outside.rs", "semantic_element_type": "function", "semantic_element_name": "outside"
    }));
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-source", "semantic_element_id": "fn:parse",
        "artifact_kind": "source", "title": "Parse source", "content": "parse source"
    }, {
        "artifact_id": "outside-source", "semantic_element_id": "fn:outside",
        "artifact_kind": "source", "title": "Outside source", "content": "outside source"
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest"),
    );
    for (kind, descendants, expected) in [
        ("elements", false, 1),
        ("elements", true, 3),
        ("relationships", false, 1),
        ("relationships", true, 4),
        ("artifacts", false, 0),
        ("artifacts", true, 1),
    ] {
        broker.reset_project_snapshot_calls();
        let result = success(runtime.invoke(PLUGIN_ID, SEMANTIC_CONTEXT_EXPORT_ID,
            serde_json::json!({"project_root": "/work/demo", "root_element_id": "file:parser",
                "record_kind": kind, "include_descendants": descendants})).expect("context"));
        assert_eq!(
            result[kind].as_array().unwrap().len(),
            expected,
            "{kind}, descendants={descendants}"
        );
        for other in ["elements", "relationships", "artifacts"]
            .into_iter()
            .filter(|other| *other != kind)
        {
            assert_eq!(result[other], serde_json::json!([]), "unrequested {other}");
        }
        assert!(result["commit_version"].as_i64().unwrap() > 0);
        if kind == "elements" {
            assert_eq!(
                broker.project_snapshot_calls(),
                0,
                "scoped element context must not read the whole project"
            );
            if descendants {
                let child = result["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["semantic_element_id"] == "fn:parse")
                    .unwrap();
                assert_eq!(
                    child["metadata"]["indexer_metadata"]["anchor_selector"]["exact"],
                    "Calibration passage"
                );
            }
        }
    }
    let unknown = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "root_element_id": "missing:root",
            "record_kind": "elements"}),
            )
            .expect("unknown root"),
    );
    assert!(unknown.message.contains("missing:root"));
    let mut cycle = index_batch();
    cycle["semantic_relationships"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "source_element_id": "fn:parse", "target_element_id": "file:parser",
            "relationship_kind": "contains", "relationship_label": "contains"
        }));
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, cycle)
            .expect("cyclic containment"),
    );
    let cyclic = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "root_element_id": "file:parser",
            "record_kind": "elements"}),
            )
            .expect("default descendant scope terminates"),
    );
    let ids: HashSet<&str> = cyclic["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|element| element["semantic_element_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains("fn:parse"));
    assert!(!ids.contains("fn:outside"));
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn deep_element_context_preserves_all_descendants_and_project_isolation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[74; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"] = serde_json::json!((0..=130).map(|index| serde_json::json!({
        "semantic_source_id":"source-main", "semantic_element_id":format!("section:{index}"),
        "path":"deep.md", "semantic_element_type":"markdown_section", "semantic_element_name":format!("Section {index}"),
        "parent_element_id":(index > 0).then(|| format!("section:{}", index - 1))
    })).collect::<Vec<_>>());
    batch["semantic_relationships"] = serde_json::json!([]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest deep tree"),
    );
    broker.reset_project_snapshot_calls();
    let result = success(runtime.invoke(PLUGIN_ID, SEMANTIC_CONTEXT_EXPORT_ID,
        serde_json::json!({"project_root":"/work/demo", "root_element_id":"section:0", "record_kind":"elements"})).expect("deep context"));
    assert_eq!(result["elements"].as_array().unwrap().len(), 131);
    assert_eq!(
        broker.project_snapshot_calls(),
        1,
        "depth boundary must fall back to a complete snapshot"
    );
    let wrong_project = failure(runtime.invoke(PLUGIN_ID, SEMANTIC_CONTEXT_EXPORT_ID,
        serde_json::json!({"project_root":"/work/other", "root_element_id":"section:0", "record_kind":"elements"})).expect("foreign root"));
    assert!(wrong_project.message.contains("requested project"));
    runtime.stop(PLUGIN_ID).expect("stop");
}
