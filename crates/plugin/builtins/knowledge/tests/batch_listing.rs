use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};

struct ScopedArtifactStorage {
    calls: usize,
}

impl HostCallTransport for ScopedArtifactStorage {
    fn host_call(&mut self, capability: &str, request: Value) -> Result<Value, PluginError> {
        self.calls += 1;
        assert_eq!(capability, "storage.semantic");
        assert_eq!(request["operation"], "artifacts_for_elements");
        let mut ids = request["semantic_element_ids"].as_array().unwrap().clone();
        ids.sort_by_key(Value::to_string);
        assert_eq!(ids, vec![json!("child"), json!("file")]);
        Ok(json!({"artifacts": [record("child", "/demo"), record("file", "/other")]}))
    }
}

fn record(owner: &str, project: &str) -> Value {
    json!({"artifact_id":format!("note-{owner}"),"semantic_element_id":owner,
        "artifact_kind":"annotation","title":"Note","content":"Content","dependencies":[],
        "metadata":{"knowledge":{"tags":["demo"],"metadata":{"retained":true},
            "path":null,"project_root":project}}})
}

#[test]
fn scoped_listing_batches_deduplicated_owners_and_retains_project_filter() {
    let mut storage = ScopedArtifactStorage { calls: 0 };
    let output = KnowledgePlugin::default()
        .dispatch(
            "list_knowledge",
            json!({"project_root":"/demo","semantic_element_ids":["file","child","file"]}),
            &mut PluginContext::for_test(&mut storage),
        )
        .unwrap();
    assert_eq!(storage.calls, 1);
    assert_eq!(output["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(output["artifacts"][0]["semantic_element_id"], "child");
    assert_eq!(output["artifacts"][0]["metadata"]["retained"], true);
}

#[test]
fn empty_scope_never_falls_back_to_project_or_global_scan() {
    let mut storage = ScopedArtifactStorage { calls: 0 };
    let output = KnowledgePlugin::default()
        .dispatch(
            "list_knowledge",
            json!({"semantic_element_ids":[]}),
            &mut PluginContext::for_test(&mut storage),
        )
        .unwrap();
    assert_eq!(output, json!({"artifacts":[]}));
    assert_eq!(storage.calls, 0);
    assert!(
        KnowledgePlugin::default()
            .dispatch(
                "list_knowledge",
                json!({"semantic_element_ids":[""]}),
                &mut PluginContext::for_test(&mut storage)
            )
            .is_err()
    );
    assert_eq!(storage.calls, 0);
}
