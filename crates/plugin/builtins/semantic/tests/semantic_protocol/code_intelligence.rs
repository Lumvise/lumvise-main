use super::*;
use serde_json::json;

#[test]
fn release_template_matches_the_compiled_capability_contract() {
    let mut manifest = package_manifest_source("test-target", &"a".repeat(64));
    manifest.targets.clear();
    manifest.files.clear();
    let template: serde_json::Value =
        serde_json::from_str(include_str!("../../lumvise-plugin-manifest.json")).unwrap();
    assert_eq!(serde_json::to_value(manifest).unwrap(), template);
}

#[test]
fn code_intelligence_exports_cross_signed_protobuf_and_host_boundaries() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("src")).unwrap();
    std::fs::write(
        workspace.path().join("src/parser.rs"),
        "fn parse() {}\n".repeat(80),
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("src/render.rs"),
        "fn render() {}\n".repeat(80),
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("src/image.png"),
        [0x89, 0x50, 0x4e, 0x47],
    )
    .unwrap();
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[48; 32]));
    let runtime = system(Arc::new(MemoryCapabilityBroker::default()));
    runtime.install(&installed).unwrap();
    runtime.start(PLUGIN_ID).unwrap();
    let root = workspace.path().to_str().unwrap();
    let mut batch = index_batch();
    batch["project_root"] = json!(root);
    batch["semantic_elements"][0]["metadata"] = json!({"extraction":{"status":"parsed","resolution":{"resolved":2,"unresolved":1,"ambiguous":0}}});
    batch["semantic_elements"].as_array_mut().unwrap().extend([
        json!({"semantic_source_id":"source-main","semantic_element_id":"file:image","path":"src/image.png",
            "semantic_element_type":"file","semantic_element_name":"image.png","metadata":{"extraction":{"status":"binary"}}}),
        json!({"semantic_source_id":"source-main","semantic_element_id":"fn:unused-a","path":"src/parser.rs",
            "semantic_element_type":"function","semantic_element_name":"unused a","start_line":40,"end_line":42}),
        json!({"semantic_source_id":"source-main","semantic_element_id":"fn:unused-b","path":"src/parser.rs",
            "semantic_element_type":"function","semantic_element_name":"unused b","start_line":50,"end_line":52}),
    ]);
    batch["semantic_elements"].as_array_mut().unwrap().extend((0..32).map(|index| {
        json!({"semantic_source_id":"source-main","semantic_element_id":format!("file:missing-{index}"),
            "path":format!("src/missing/failure-{index}.rs"),"semantic_element_type":"file",
            "semantic_element_name":format!("failure-{index}.rs")})
    }));
    success(runtime.invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch).unwrap());
    let invoke = |id: &str, mut input: serde_json::Value| {
        input["project_root"] = json!(root);
        success(runtime.invoke(PLUGIN_ID, id, input).unwrap())
    };
    assert_eq!(
        invoke("search_graph", json!({"query":"render"}))["matches"][0]["element"]["semantic_element_id"],
        "fn:render"
    );
    assert_eq!(
        invoke("get_graph_schema", json!({}))["element_kinds"]["function"],
        4
    );
    assert_eq!(
        invoke(
            "get_code_snippet",
            json!({"semantic_element_id":"fn:parse"})
        )["start_line"],
        10
    );
    let search = invoke("search_code", json!({"query":"render","limit":1}));
    assert!(search["total"].as_u64().unwrap() > 0);
    assert_eq!(search["matches"].as_array().unwrap().len(), 1);
    let failures = search["failures"].as_array().unwrap();
    assert_eq!(
        search["failure_count"].as_u64().unwrap() as usize,
        failures.len()
    );
    let missing_failure = failures
        .iter()
        .find(|failure| failure["path"] == "src/missing/failure-0.rs")
        .unwrap();
    assert!(
        missing_failure["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
    assert_eq!(
        invoke("trace_path", json!({"semantic_element_id":"fn:parse"}))["elements"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let metrics = invoke("get_graph_metrics", json!({"limit":1}));
    assert_eq!(metrics["cycles"]["total"], 1);
    assert!(metrics["cycles"]["next_offset"].is_null());
    assert_eq!(metrics["dead_code_candidates"]["total"], 2);
    assert_eq!(metrics["dead_code_candidates"]["next_offset"], 1);
    assert_eq!(
        metrics["dead_code_candidates"]["matches"][0]
            .as_object()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        invoke("check_index_coverage", json!({}))["status_counts"]["parsed"],
        1
    );
    let observation = json!({"trace_id":"fixture","edges":[{"source_element_id":"fn:render","target_element_id":"fn:parse"}]});
    assert_eq!(
        invoke("ingest_runtime_trace", observation)["accepted"],
        true
    );
    let trace = invoke("trace_path", json!({"semantic_element_id":"fn:render"}));
    assert!(
        trace["relationships"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["metadata"]["origin"] == "runtime")
    );
    runtime.stop(PLUGIN_ID).unwrap();
}
