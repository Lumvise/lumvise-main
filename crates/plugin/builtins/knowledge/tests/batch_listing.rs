use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};

struct ScopedArtifactStorage {
    /// Every host call in order with the owner or artifact IDs it asked about.
    calls: Vec<(String, Value)>,
}

impl HostCallTransport for ScopedArtifactStorage {
    fn host_call(&mut self, capability: &str, request: Value) -> Result<Value, PluginError> {
        assert_eq!(capability, "storage.semantic");
        let mut ids = request["semantic_element_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        ids.sort_by_key(Value::to_string);
        let operation = request["operation"].as_str().unwrap_or_default().to_owned();
        let asked = match operation.as_str() {
            "revision_selection_members" => request["query"]["artifact_ids"].clone(),
            _ => json!(ids),
        };
        self.calls.push((operation, asked));
        match request["operation"].as_str() {
            Some("semantic_revision") => Ok(json!({"commit_version": 7})),
            Some("revision_selection_members") => {
                assert_eq!(request["query"]["project_root"], "/demo");
                Ok(json!({"membership": null}))
            }
            Some("artifacts_for_elements") => {
                Ok(json!({"artifacts": [record("child", "/old-route"), record("file", "/demo")]}))
            }
            Some("elements_by_ids_including_inactive") => {
                assert_eq!(request["project_root"], "/demo");
                Ok(
                    json!({"elements":[{"project_root":"/demo","semantic_element_id":"child","semantic_source_id":"source",
                    "path":"src/lib.rs","element_kind":"function","name":"Child","parent_element_id":null,
                    "content_fingerprint":null,"start_line":null,"end_line":null,"lifecycle":"active","metadata":{}}]}),
                )
            }
            other => panic!(
                "unexpected read {other:?}; expected batched owner/version reads and a commit fence"
            ),
        }
    }
}

fn record(owner: &str, project: &str) -> Value {
    json!({"artifact_id":format!("note-{owner}"),"semantic_element_id":owner,
        "artifact_kind":"specification","title":"Note","content":"Content","dependencies":[],
        "metadata":{"knowledge":{"tags":["demo"],"metadata":{"retained":true},
            "path":null,"project_root":project}}})
}

#[test]
fn scoped_listing_batches_deduplicated_owners_and_retains_project_filter() {
    let mut storage = ScopedArtifactStorage { calls: vec![] };
    let output = KnowledgePlugin::default()
        .dispatch(
            "list_knowledge",
            json!({"project_root":"/demo","semantic_element_ids":["file","child","file"]}),
            &mut PluginContext::for_test(&mut storage),
        )
        .unwrap();
    // One batched read per kind for the deduplicated owners, then one staleness
    // pass (selection probe + exact anchor lookup) for the single returned note.
    assert_eq!(
        storage.calls,
        [
            ("semantic_revision", json!([])),
            ("artifacts_for_elements", json!(["child", "file"])),
            (
                "elements_by_ids_including_inactive",
                json!(["child", "file"])
            ),
            ("revision_selection_members", json!(["note-child"])),
            ("semantic_revision", json!([])),
            ("revision_selection_members", json!([])),
            ("elements_by_ids_including_inactive", json!(["child"])),
        ]
        .map(|(operation, asked)| (operation.to_owned(), asked))
    );
    assert_eq!(output["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(output["artifacts"][0]["semantic_element_id"], "child");
    assert_eq!(output["artifacts"][0]["project_root"], "/demo");
    assert_eq!(output["artifacts"][0]["metadata"]["retained"], true);
}

#[test]
fn empty_scope_never_falls_back_to_project_or_global_scan() {
    let mut storage = ScopedArtifactStorage { calls: vec![] };
    let output = KnowledgePlugin::default()
        .dispatch(
            "list_knowledge",
            json!({"semantic_element_ids":[]}),
            &mut PluginContext::for_test(&mut storage),
        )
        .unwrap();
    assert_eq!(output, json!({"artifacts":[]}));
    assert!(storage.calls.is_empty());
    assert!(
        KnowledgePlugin::default()
            .dispatch(
                "list_knowledge",
                json!({"semantic_element_ids":[""]}),
                &mut PluginContext::for_test(&mut storage)
            )
            .is_err()
    );
    assert!(storage.calls.is_empty());
}
