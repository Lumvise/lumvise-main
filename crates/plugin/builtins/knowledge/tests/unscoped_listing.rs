//! One unreadable online project must not hide the knowledge of the projects that can be read.
use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};

const ONLINE: &str = "lumvise://projects/22222222-2222-4222-8222-222222222222";

struct Projects {
    /// Roots whose artifact listing fails.
    unreadable: Vec<&'static str>,
    /// The roots whose listing was requested, in order.
    listed: Vec<String>,
}

impl HostCallTransport for Projects {
    fn host_call(&mut self, capability: &str, request: Value) -> Result<Value, PluginError> {
        assert_eq!(capability, "storage.semantic");
        match request["operation"].as_str().unwrap_or_default() {
            "project_roots" => Ok(json!({"project_roots":["/local", ONLINE]})),
            "project_artifacts" => {
                let root = request["project_root"].as_str().unwrap().to_owned();
                self.listed.push(root.clone());
                if self.unreadable.contains(&root.as_str()) {
                    return Err(PluginError::new(
                        "host_capability_execution_failed",
                        format!(
                            "storage.semantic operation failed: invalid value {root}; expected project_too_large: this project is too large for this view"
                        ),
                        false,
                    ));
                }
                Ok(json!({"artifacts":[note(&root)]}))
            }
            "semantic_revision" => Ok(json!({"commit_version": 7})),
            "revision_selection_members" => Ok(json!({"membership": null})),
            "elements_by_ids_including_inactive" => Ok(json!({"elements": []})),
            other => panic!("unexpected read {other}; expected a project listing"),
        }
    }
}

fn note(root: &str) -> Value {
    json!({"artifact_id":format!("note-{root}"), "semantic_element_id":"file",
        "artifact_kind":"specification", "title":"Note", "content":"Content", "dependencies":[],
        "metadata":{"knowledge":{"tags":[], "metadata":{}, "path":null, "project_root":root}}})
}

fn list_everything(host: &mut Projects) -> Result<Value, PluginError> {
    KnowledgePlugin::default().dispatch(
        "list_knowledge",
        json!({}),
        &mut PluginContext::for_test(host),
    )
}

#[test]
fn an_unreadable_online_project_is_skipped_and_the_local_knowledge_is_listed() {
    let mut host = Projects {
        unreadable: vec![ONLINE],
        listed: vec![],
    };
    let output = list_everything(&mut host).unwrap();
    assert_eq!(
        host.listed,
        ["/local", ONLINE],
        "the online project was tried"
    );
    let artifacts = output["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0]["project_root"], "/local");
}

#[test]
fn a_readable_online_project_is_listed_with_the_local_ones() {
    let mut host = Projects {
        unreadable: vec![],
        listed: vec![],
    };
    let output = list_everything(&mut host).unwrap();
    let mut roots = output["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| artifact["project_root"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    roots.sort();
    assert_eq!(roots, ["/local", ONLINE]);
}

#[test]
fn an_unreadable_local_project_still_fails_the_listing() {
    let mut host = Projects {
        unreadable: vec!["/local"],
        listed: vec![],
    };
    let error = list_everything(&mut host).unwrap_err();
    assert_eq!(error.code, "host_capability_execution_failed");
    assert_eq!(
        host.listed,
        ["/local"],
        "a local failure stops before the next project"
    );
}
