use super::*;
use crate::SemanticPlugin;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication};

#[test]
fn signed_template_matches_source_controls_and_required_host_version() {
    let template: Value =
        serde_json::from_str(include_str!("../../lumvise-plugin-manifest.json")).unwrap();
    let manifest = crate::package_manifest_source("test", &"0".repeat(64));
    assert_eq!(
        template["exports"],
        serde_json::to_value(&manifest.exports).unwrap()
    );
    assert_eq!(
        template["host_capabilities"],
        serde_json::to_value(&manifest.host_capabilities).unwrap()
    );
}

struct FakeRevisionControlHost {
    project_id: String,
    rows: std::collections::BTreeMap<String, Value>,
    staged: Vec<Value>,
    calls: Vec<Value>,
    reject_commit: bool,
}

impl FakeRevisionControlHost {
    fn new() -> Self {
        Self {
            project_id: "1775f5a0-a1e7-46ce-92ea-b1b5cec4c24a".into(),
            rows: Default::default(),
            staged: vec![],
            calls: vec![],
            reject_commit: false,
        }
    }
    fn invoke(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        SemanticPlugin.dispatch(export, input, &mut PluginContext::for_test(self))
    }
    fn commit(&mut self) -> Result<Value, PluginError> {
        if self.reject_commit {
            return Err(PluginError::new(
                "storage_conflict",
                "injected concurrent preference update",
                false,
            ));
        }
        for mutation in &self.staged {
            let key = mutation["row_key"].as_str().unwrap();
            if mutation["operation"] == "require_value" {
                let expected = &mutation["expected"];
                match self.rows.get(key) {
                    None => assert_eq!(expected["presence"], "absent"),
                    Some(stored) => assert_eq!(&expected["value"], stored),
                }
            }
        }
        for mutation in self.staged.drain(..).filter(|m| m["operation"] == "put") {
            self.rows.insert(
                mutation["row_key"].as_str().unwrap().into(),
                mutation["value"].clone(),
            );
        }
        Ok(json!({}))
    }
}

impl HostCallTransport for FakeRevisionControlHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.calls.push(input.clone());
        match (capability, input["operation"].as_str().unwrap()) {
            ("storage.semantic", "project_identity") => {
                Ok(json!({"identity":{"project_id":self.project_id}}))
            }
            ("storage.plugin", "ensure_table") => Ok(json!({})),
            ("storage.plugin", "get_row") => Ok(
                json!({"row":self.rows.get(input["row_key"].as_str().unwrap()).map(|value| json!({"value":value}))}),
            ),
            ("storage.plugin", "begin_mutations") => {
                self.staged.clear();
                Ok(json!({"transaction_id":"test"}))
            }
            ("storage.plugin", "stage_mutations") => {
                self.staged
                    .extend(input["mutations"].as_array().unwrap().iter().cloned());
                Ok(json!({}))
            }
            ("storage.plugin", "commit_mutations") => self.commit(),
            other => panic!("unexpected source control host call: {other:?}"),
        }
    }
}

#[test]
fn preferences_use_lineage_not_path_and_never_schedule_or_collect() {
    let mut host = FakeRevisionControlHost::new();
    let default = host
        .invoke(
            "get_revision_retention_preference",
            json!({"project_root":"/p"}),
        )
        .unwrap();
    assert_eq!(
        default["preference"]["policy"],
        json!({"mode":"age", "history_age_seconds":2592000})
    );
    assert_eq!(default["automatic_collection"], false);
    let saved = host.invoke("set_revision_retention_preference", json!({"project_root":"/p", "expected_revision":0, "policy":{"mode":"revision_count", "revisions":10}})).unwrap();
    assert_eq!(saved["preference"]["revision"], 1);
    assert_eq!(
        host.invoke(
            "get_revision_retention_preference",
            json!({"project_root":"/alias"})
        )
        .unwrap(),
        saved
    );
    host.project_id = "2c57518f-faf5-4ed7-bdbe-e6f3d8d76fb2".into();
    assert_eq!(
        host.invoke(
            "get_revision_retention_preference",
            json!({"project_root":"/p"})
        )
        .unwrap()["preference"]["revision"],
        0
    );
    assert!(host.calls.iter().all(|call| {
        !["collect_revision_retention", "plan_revision_retention"]
            .contains(&call["operation"].as_str().unwrap())
    }));
}

#[test]
fn preference_validation_and_conditional_commit_preserve_existing_choice() {
    let mut host = FakeRevisionControlHost::new();
    for policy in [
        json!({"mode":"revision_count", "revisions":0}),
        json!({"mode":"age", "history_age_seconds":0}),
    ] {
        assert!(
            host.invoke(
                "set_revision_retention_preference",
                json!({"project_root":"/p", "expected_revision":0, "policy":policy})
            )
            .is_err()
        );
    }
    let input = json!({"project_root":"/p", "expected_revision":0, "policy":{"mode":"revision_count", "revisions":10}});
    host.reject_commit = true;
    assert!(
        host.invoke("set_revision_retention_preference", input.clone())
            .is_err()
    );
    assert!(host.rows.is_empty());
    host.reject_commit = false;
    let saved = host
        .invoke("set_revision_retention_preference", input.clone())
        .unwrap();
    assert_eq!(
        host.invoke("set_revision_retention_preference", input)
            .unwrap_err()
            .code,
        "retention_preference_conflict"
    );
    assert_eq!(
        host.invoke(
            "get_revision_retention_preference",
            json!({"project_root":"/p"})
        )
        .unwrap(),
        saved
    );
}

#[test]
fn source_controls_preserve_frozen_selection_and_collection_inputs() {
    let update = json!({"operation_id":"pin-1", "project_root":"/project",
        "project_id":"1775f5a0-a1e7-46ce-92ea-b1b5cec4c24a", "selection_id":"review",
        "source_context":"local", "expected_generation":4, "artifact_edits":{"a1":3},
        "mode":"pinned", "records":[{"kind":"artifact", "record_id":"a1"}],
        "connection_ids":[], "source_anchor":null});
    assert_eq!(
        request("select_revisions", update.clone()).unwrap(),
        json!({"operation":"select_revisions", "update":update})
    );
    let plan = json!({"operation_id":"sweep-1", "project_root":"/project",
        "project_id":"1775f5a0-a1e7-46ce-92ea-b1b5cec4c24a", "expected_commit_version":9,
        "evaluated_at_ms":1000, "records":[], "expired_association_ids":[]});
    let policy = json!({"mode":"revision_count", "revisions":10});
    assert_eq!(
        request(
            "collect_revision_retention",
            json!({"plan":plan,"policy":policy})
        )
        .unwrap(),
        json!({"operation":"collect_revision_retention","plan":plan,"policy":policy})
    );
}

#[test]
fn source_controls_reject_missing_scope_unknown_controls_and_extra_fields() {
    for input in [
        json!({"project_root":"", "selection_id":"working"}),
        json!({"project_root":"/p", "selection_id":"working", "automatic":true}),
    ] {
        assert!(request("read_revision_selection", input).is_err());
    }
    assert!(request("select_latest_automatically", json!({})).is_err());
    assert!(request("preview_revision_retention", json!({"project_root":"/p", "operation_id":"", "policy":{"mode":"age", "history_age_seconds":30}})).is_err());
}

#[test]
fn generic_selection_controls_cannot_create_change_or_release_citation_claims() {
    for records in [
        json!([]),
        json!([{"kind":"artifact", "record_id":"evidence"}]),
    ] {
        for mode in ["pinned", "follow_accepted"] {
            let input = json!({"operation_id":"edit-claim", "project_root":"/p",
                "project_id":"1775f5a0-a1e7-46ce-92ea-b1b5cec4c24a", "selection_id":"knowledge-citation:another-owner",
                "source_context":"local", "expected_generation":0, "artifact_edits":{},
                "mode":mode, "records":records, "connection_ids":[], "source_anchor":null});
            assert_eq!(
                request("select_revisions", input).unwrap_err().code,
                "reserved_revision_selection"
            );
        }
    }
}

#[test]
fn exact_reads_and_preview_keep_the_source_identity_and_policy() {
    let exact = json!({"project_root":"/p", "record":{"kind":"element", "record_id":"e17"}});
    assert_eq!(
        request("resolve_revision", exact).unwrap()["record"]["record_id"],
        "e17"
    );
    let selection = request(
        "read_revision_selection",
        json!({"project_root":"/p", "selection_id":"public"}),
    )
    .unwrap();
    assert_eq!(selection["selection_id"], "public");
    let policy = json!({"mode":"age", "history_age_seconds":2592000});
    let preview = request(
        "preview_revision_retention",
        json!({"project_root":"/p", "operation_id":"preview", "policy":policy}),
    )
    .unwrap();
    assert_eq!(preview["policy"], policy);
    assert_eq!(preview["operation"], "plan_revision_retention");
}
