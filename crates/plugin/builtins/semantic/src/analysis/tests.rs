use crate::SemanticPlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct FakeCodeHost {
    snapshot: Value,
    rows: BTreeMap<String, Value>,
    calls: Vec<(String, Value)>,
}

impl FakeCodeHost {
    fn new() -> Self {
        let elements:Vec<_> = [("file","demo.rs","file",1,20),("a","alpha","function",1,3),("b","beta","function",5,8),
            ("c","gamma","function",10,13),("d","unused","function",15,18),
            ("binary","image.png","file",1,1),("unsupported","manual.pdf","file",1,1)].into_iter().map(|(id,name,kind,start,end)|
                json!({"semantic_element_id":id,"name":name,"element_kind":kind,"path":"demo.rs",
                    "parent_element_id":null,"start_line":start,"end_line":end,"content_fingerprint":"fingerprint",
                    "metadata":{"indexer_metadata":{"extraction":{"status":"parsed","resolution":{"unresolved":1}}}}})).collect();
        let mut elements = elements;
        elements[5]["path"] = json!("image.png");
        elements[5]["metadata"]["indexer_metadata"]["extraction"]["status"] = json!("binary");
        elements[6]["path"] = json!("manual.pdf");
        elements[6]["metadata"]["indexer_metadata"]["extraction"]["status"] = json!("unsupported");
        let relationships:Vec<_> = [("a","b","calls"),("b","a","calls"),("b","c","calls"),("c","d","uses_type")]
            .into_iter().map(|(source,target,label)| json!({"source_element_id":source,"target_element_id":target,"relationship_kind":"semantic","label":label})).collect();
        Self {
            snapshot: json!({"project_root":"/project","commit_version":7,"published_at":"now","elements":elements,"relationships":relationships,"artifacts":[]}),
            rows: BTreeMap::new(),
            calls: Vec::new(),
        }
    }
    fn invoke(&mut self, id: &str, mut input: Value) -> Result<Value, PluginError> {
        input["project_root"] = json!("/project");
        SemanticPlugin.dispatch(id, input, &mut PluginContext::for_test(self))
    }
}

impl HostCallTransport for FakeCodeHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.calls.push((capability.into(), input.clone()));
        match (capability, input["operation"].as_str().unwrap()) {
            ("storage.semantic", "project_snapshot") => Ok(self.snapshot.clone()),
            ("storage.plugin", "ensure_table") => Ok(json!({})),
            ("storage.plugin", "list_rows") => Ok(
                json!({"rows":self.rows.iter().map(|(key,value)|json!({"row_key":key,"value":value})).collect::<Vec<_>>(),"next_after_key":null}),
            ),
            ("storage.plugin", "put_row") => {
                self.rows.insert(
                    input["row_key"].as_str().unwrap().into(),
                    input["value"].clone(),
                );
                Ok(json!({}))
            }
            ("project.source", "read") => Ok(
                json!({"text":"fn alpha() {}","path":"demo.rs","source_fingerprint":"fingerprint"}),
            ),
            ("project.source", "search") => Ok(
                json!({"matches":[{"path":"demo.rs","line":6,"text":"alpha()"},{"path":"demo.rs","line":2,"text":"alpha()"}],"failures":[]}),
            ),
            ("project.source", "git_changes") => Ok(
                json!({"base_commit":"abc","changes":[{"path":"demo.rs","status":"M"},{"path":"new.rs","status":"?"}]}),
            ),
            _ => panic!("unexpected host call {capability}: {input}"),
        }
    }
}

#[test]
fn symbol_search_filters_ranks_paginates_and_rejects_stale_pages() {
    let mut host = FakeCodeHost::new();
    let result = host
        .invoke(
            "search_graph",
            json!({"element_kind":"function","name_pattern":"alpha|beta","limit":1}),
        )
        .unwrap();
    assert_eq!(result["total"], 2);
    assert_eq!(result["next_offset"], 1);
    assert_eq!(result["matches"][0]["element"]["name"], "alpha");
    let next=host.invoke("search_graph",json!({"element_kind":"function","name_pattern":"alpha|beta","limit":1,"offset":1,"expected_commit_version":7})).unwrap();
    assert_eq!(next["matches"][0]["element"]["name"], "beta");
    assert_eq!(next["next_offset"], Value::Null);
    assert!(
        host.invoke("search_graph", json!({"expected_commit_version":6}))
            .is_err()
    );
    assert!(
        host.invoke("search_graph", json!({"name_pattern":"["}))
            .is_err()
    );
    assert_eq!(
        host.invoke("search_graph", json!({"path_pattern":"missing"}))
            .unwrap()["total"],
        0
    );
}

#[test]
fn filtered_trace_stops_at_depth_and_handles_cycles_in_both_directions() {
    let mut host = FakeCodeHost::new();
    let shallow = host
        .invoke(
            "trace_path",
            json!({"semantic_element_id":"a","max_depth":1}),
        )
        .unwrap();
    assert_eq!(shallow["elements"].as_array().unwrap().len(), 2);
    assert_eq!(shallow["truncated"], true);
    let inbound = host
        .invoke(
            "trace_path",
            json!({"semantic_element_id":"c","direction":"inbound"}),
        )
        .unwrap();
    assert_eq!(inbound["elements"].as_array().unwrap().len(), 3);
    let uses = host
        .invoke(
            "trace_path",
            json!({"semantic_element_id":"c","relationship_labels":["uses_type"]}),
        )
        .unwrap();
    assert_eq!(uses["relationships"][0]["target_element_id"], "d");
    assert!(
        host.invoke("trace_path", json!({"semantic_element_id":"missing"}))
            .is_err()
    );
}

#[test]
fn schema_metrics_and_coverage_report_observed_evidence() {
    let mut host = FakeCodeHost::new();
    let schema = host.invoke("get_graph_schema", json!({})).unwrap();
    assert_eq!(schema["element_kinds"]["function"], 4);
    let metrics = host.invoke("get_graph_metrics", json!({})).unwrap();
    assert_eq!(metrics["cycles"]["matches"], json!([["a", "b"]]));
    assert_eq!(metrics["cycles"]["total"], 1);
    assert_eq!(metrics["edge_count"], 3);
    assert_eq!(
        metrics["hotspots"]["matches"][0]["semantic_element_id"],
        "b"
    );
    assert_eq!(
        metrics["dead_code_candidates"]["matches"][0]["semantic_element_id"],
        "d"
    );
    assert_eq!(
        metrics["dead_code_candidates"]["matches"][0]
            .as_object()
            .unwrap()
            .len(),
        5
    );
    let coverage = host.invoke("check_index_coverage", json!({})).unwrap();
    assert_eq!(coverage["status_counts"]["parsed"], 1);
    host.snapshot["elements"][0]["metadata"] = Value::Null;
    assert_eq!(
        host.invoke("check_index_coverage", json!({})).unwrap()["status_counts"]["unknown"],
        1
    );
}

#[test]
fn graph_metrics_pages_cycles_and_compact_dead_code_rows() {
    let mut host = FakeCodeHost::new();
    host.snapshot["elements"].as_array_mut().unwrap().extend([
        json!({"semantic_element_id":"e","name":"echo","element_kind":"function","path":"demo.rs","start_line":19,"end_line":20}),
        json!({"semantic_element_id":"f","name":"foxtrot","element_kind":"function","path":"demo.rs","start_line":21,"end_line":22}),
        json!({"semantic_element_id":"g","name":"golf","element_kind":"function","path":"demo.rs","start_line":23,"end_line":24}),
    ]);
    host.snapshot["relationships"].as_array_mut().unwrap().extend([
        json!({"source_element_id":"e","target_element_id":"f","relationship_kind":"semantic","label":"calls"}),
        json!({"source_element_id":"f","target_element_id":"e","relationship_kind":"semantic","label":"calls"}),
    ]);
    let first = host
        .invoke("get_graph_metrics", json!({"limit":1}))
        .unwrap();
    assert_eq!(first["cycles"]["total"], 2);
    assert_eq!(first["cycles"]["next_offset"], 1);
    assert_eq!(first["cycles"]["matches"], json!([["a", "b"]]));
    assert_eq!(first["dead_code_candidates"]["total"], 2);
    assert_eq!(first["dead_code_candidates"]["next_offset"], 1);
    assert_eq!(
        first["dead_code_candidates"]["matches"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "element_kind",
            "name",
            "path",
            "semantic_element_id",
            "start_line"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    let next = host
        .invoke("get_graph_metrics", json!({"limit":1,"offset":1}))
        .unwrap();
    assert_eq!(next["cycles"]["matches"], json!([["e", "f"]]));
    assert_eq!(next["cycles"]["next_offset"], Value::Null);
    assert_eq!(
        next["dead_code_candidates"]["matches"][0]["semantic_element_id"],
        "g"
    );
    assert_eq!(next["dead_code_candidates"]["next_offset"], Value::Null);
    assert!(first["caveat"].as_str().is_some());
}

#[test]
fn source_tools_use_host_boundary_and_preserve_symbol_context() {
    let mut host = FakeCodeHost::new();
    let snippet = host
        .invoke(
            "get_code_snippet",
            json!({"semantic_element_id":"a","context_lines":2}),
        )
        .unwrap();
    assert_eq!(snippet["index_matches_source"], true);
    assert_eq!(host.calls.last().unwrap().1["start_line"], 1);
    assert_eq!(host.calls.last().unwrap().1["end_line"], 5);
    let search = host
        .invoke("search_code", json!({"query":"alpha"}))
        .unwrap();
    assert_eq!(search["matches"][0]["element"]["semantic_element_id"], "a");
    let source_search = host
        .calls
        .iter()
        .find(|(capability, input)| {
            capability == "project.source" && input["operation"] == "search"
        })
        .unwrap();
    let paths = source_search.1["paths"].as_array().unwrap();
    assert!(paths.iter().any(|path| path == "demo.rs"));
    assert!(
        !paths
            .iter()
            .any(|path| path == "image.png" || path == "manual.pdf")
    );
    let impact = host.invoke("get_git_impact", json!({})).unwrap();
    assert_eq!(impact["unindexed_paths"], json!(["new.rs"]));
    assert_eq!(impact["direction"], "inbound");
}

#[test]
fn runtime_trace_replacement_is_idempotent_and_never_replaces_static_graph() {
    let mut host = FakeCodeHost::new();
    let input = json!({"trace_id":"test-run","edges":[{"source_element_id":"c","target_element_id":"d","count":2}]});
    host.invoke("ingest_runtime_trace", input.clone()).unwrap();
    host.invoke("ingest_runtime_trace", input).unwrap();
    assert_eq!(host.rows.len(), 1);
    let trace = host
        .invoke("trace_path", json!({"semantic_element_id":"c"}))
        .unwrap();
    assert_eq!(trace["relationships"][0]["metadata"]["origin"], "runtime");
    assert_eq!(trace["relationships"][0]["metadata"]["count"], 2);
    assert!(
        host.calls
            .iter()
            .all(|(_, input)| input["operation"] != "sync_structure")
    );
    assert!(
        host.invoke(
            "ingest_runtime_trace",
            json!({"trace_id":"bad","edges":[{"source_element_id":"x","target_element_id":"d"}]})
        )
        .is_err()
    );
    host.invoke(
        "ingest_runtime_trace",
        json!({"trace_id":"test-run","edges":[]}),
    )
    .unwrap();
    assert_eq!(
        host.invoke("trace_path", json!({"semantic_element_id":"c"}))
            .unwrap()["relationships"],
        json!([])
    );
}
