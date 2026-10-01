use super::*;
use crate::{
    KnowledgePlugin,
    functional::{self, FunctionalSummary},
    manifest::{ELEMENT_TRIGGER_EXPORT_ID, ENSURE_C4_EXPORT_ID},
};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
struct SchedulingFakeHost {
    elements: Vec<SemanticElement>,
    relationships: Vec<semantic_context::SemanticRelationship>,
    artifacts: Vec<KnowledgeArtifact>,
    rows: BTreeMap<(String, String), Value>,
    listed_tables: Vec<String>,
    submitted: Vec<String>,
    rejected: HashSet<String>,
}

impl HostCallTransport for SchedulingFakeHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        match (capability, input["operation"].as_str().unwrap_or_default()) {
            ("storage.plugin", "ensure_table") => Ok(json!({})),
            ("storage.plugin", "list_rows") => {
                self.record_list_rows(&input);
                Ok(self.list_rows(&input))
            }
            ("storage.plugin", "get_row") => {
                let key = (
                    input["table_name"].as_str().unwrap_or_default().to_owned(),
                    input["row_key"].as_str().unwrap_or_default().to_owned(),
                );
                Ok(json!({"row":self.rows.get(&key).map(|value| json!({"value":value}))}))
            }
            ("storage.plugin", "put_row") => {
                let table = input["table_name"].as_str().unwrap_or_default().to_owned();
                let key = input["row_key"].as_str().unwrap_or_default().to_owned();
                self.rows.insert((table, key), input["value"].clone());
                Ok(json!({}))
            }
            ("storage.plugin", "mutate_rows") => {
                self.mutate_rows(&input);
                Ok(json!({}))
            }
            ("storage.plugin", "trim_rows_by_key") => Ok(json!({})),
            ("runtime.project_execution", "submit") => self.submit(input),
            ("storage.semantic", "project_snapshot") => Ok(self.project_snapshot()),
            ("storage.semantic", "selective_subgraph") => Ok(self.selective_subgraph(&input)),
            ("storage.semantic", "elements_by_ids_including_inactive") => {
                Ok(self.elements_by_ids(&input))
            }
            ("storage.semantic", "artifacts_for_elements") => {
                Ok(self.artifacts_for_elements(&input))
            }
            ("storage.semantic", "candidate_source_elements") => Ok(json!({"elements": []})),
            ("storage.semantic", "artifact") => Ok(self.artifact(&input)),
            ("storage.semantic", "upsert_artifact") => Ok(json!({})),
            ("storage.semantic", "remove_artifact") => Ok(json!({"removed": true})),
            (other, _) => Err(PluginError::unknown_capability(other)),
        }
    }
}

impl SchedulingFakeHost {
    fn list_rows(&self, input: &Value) -> Value {
        let table = input["table_name"].as_str().unwrap_or_default();
        let prefix = input["key_prefix"].as_str();
        let rows = self
            .rows
            .iter()
            .filter(|((stored_table, key), _)| {
                stored_table == table && prefix.is_none_or(|prefix| key.starts_with(prefix))
            })
            .map(|(_, value)| json!({"value": value}))
            .collect::<Vec<_>>();
        json!({"rows": rows, "next_after_key": null})
    }

    fn record_list_rows(&mut self, input: &Value) {
        self.listed_tables
            .push(input["table_name"].as_str().unwrap_or_default().to_owned());
    }

    fn mutate_rows(&mut self, input: &Value) {
        for mutation in input["mutations"].as_array().into_iter().flatten() {
            let table = mutation["table_name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let key = mutation["row_key"].as_str().unwrap_or_default().to_owned();
            match mutation["operation"].as_str() {
                Some("delete") => {
                    self.rows.remove(&(table, key));
                }
                Some("put") => {
                    self.rows.insert((table, key), mutation["value"].clone());
                }
                _ => {}
            }
        }
    }

    fn submit(&mut self, input: Value) -> Result<Value, PluginError> {
        let element_id = input["input"]["semantic_element_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if self.rejected.contains(&element_id) {
            return Err(PluginError::new(
                "host_capability_execution_failed",
                format!("rejected test submission for `{element_id}`"),
                false,
            ));
        }
        self.submitted.push(element_id);
        Ok(json!({"job_id": format!("job-{}", self.submitted.len())}))
    }

    fn project_snapshot(&self) -> Value {
        json!({"commit_version":1,"published_at":"test","project_root":"/repo",
            "elements":self.elements,"relationships":self.relationships,
            "artifacts":self.artifacts.iter().map(graph_artifact).collect::<Vec<_>>()})
    }

    fn selective_subgraph(&self, input: &Value) -> Value {
        json!({"subgraph":{"commit_version":1,"published_at":"test","project_root":"/repo",
            "root_element_id":input["root_element_id"],"elements":self.elements,
            "relationships":self.relationships,
            "artifacts":self.artifacts.iter().map(semantic_artifact).collect::<Vec<_>>()}})
    }

    fn elements_by_ids(&self, input: &Value) -> Value {
        let ids = input["semantic_element_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<HashSet<_>>();
        json!({"elements":self.elements.iter().filter(|item| ids.contains(item.semantic_element_id.as_str())).collect::<Vec<_>>()})
    }

    fn artifacts_for_elements(&self, input: &Value) -> Value {
        let ids = input["semantic_element_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<HashSet<_>>();
        json!({"artifacts":self.artifacts.iter()
            .filter(|item| ids.contains(item.semantic_element_id.as_str()))
            .map(graph_artifact).collect::<Vec<_>>()})
    }

    fn artifact(&self, input: &Value) -> Value {
        let artifact_id = input["artifact_id"].as_str().unwrap_or_default();
        json!({"artifact":self.artifacts.iter()
            .find(|item| item.artifact_id == artifact_id)
            .map(graph_artifact)})
    }
}

fn graph_artifact(artifact: &KnowledgeArtifact) -> Value {
    json!({"artifact_id":artifact.artifact_id,"semantic_element_id":artifact.semantic_element_id,
        "artifact_kind":artifact.knowledge_type,"title":artifact.title,"content":artifact.content,
        "dependencies":artifact.dependencies,"metadata":{"knowledge":{
            "tags":artifact.tags,"metadata":artifact.metadata,"path":artifact.path,
            "project_root":artifact.project_root}}})
}

fn semantic_artifact(artifact: &KnowledgeArtifact) -> Value {
    json!({"project_root":"/repo","artifact_id":artifact.artifact_id,
        "semantic_element_id":artifact.semantic_element_id,"artifact_kind":artifact.knowledge_type,
        "title":artifact.title,"content_ref":null,"content":artifact.content,
        "searchable_text":null,"dependencies":artifact.dependencies,"content_size_bytes":null,
        "metadata":{"knowledge":{"tags":artifact.tags,"metadata":artifact.metadata,
            "path":artifact.path,"project_root":artifact.project_root}}})
}

fn fixture() -> (
    Vec<SemanticElement>,
    Vec<semantic_context::SemanticRelationship>,
) {
    let root = element("folder:crates", "folder", "crates", "crates", None);
    let file = element(
        "file:crates/demo.ts",
        "file",
        "demo.ts",
        "crates/demo.ts",
        Some(&root.semantic_element_id),
    );
    let function = element(
        "function:crates/demo.ts:render",
        "function",
        "render",
        "crates/demo.ts",
        Some(&file.semantic_element_id),
    );
    let parameter = element(
        "parameter:crates/demo.ts:render:props",
        "parameter",
        "props",
        "crates/demo.ts",
        Some(&function.semantic_element_id),
    );
    let deep = element(
        "function:crates/demo.ts:render:helper",
        "function",
        "helper",
        "crates/demo.ts",
        Some(&function.semantic_element_id),
    );
    let external_folder = element("folder:shared", "folder", "shared", "shared", None);
    let external_file = element(
        "file:shared/paint.ts",
        "file",
        "paint.ts",
        "shared/paint.ts",
        Some(&external_folder.semantic_element_id),
    );
    let external_function = element(
        "function:shared/paint.ts:paint",
        "function",
        "paint",
        "shared/paint.ts",
        Some(&external_file.semantic_element_id),
    );
    let relationship = semantic_context::SemanticRelationship {
        project_root: "/repo".into(),
        source_element_id: function.semantic_element_id.clone(),
        target_element_id: external_function.semantic_element_id.clone(),
        relationship_kind: "semantic".into(),
        label: "calls".into(),
        lifecycle: "active".into(),
        metadata: json!({}),
    };
    (
        vec![
            root,
            file,
            function,
            parameter,
            deep,
            external_folder,
            external_file,
            external_function,
        ],
        vec![relationship],
    )
}

fn element(id: &str, kind: &str, name: &str, path: &str, parent: Option<&str>) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: path.into(),
        element_kind: kind.into(),
        name: name.into(),
        parent_element_id: parent.map(str::to_owned),
        content_fingerprint: Some("fp1:0000000000000001:current".into()),
        start_line: Some(1),
        end_line: Some(5),
        lifecycle: "active".into(),
        metadata: json!({}),
    }
}

fn functional_artifact(element: &SemanticElement) -> KnowledgeArtifact {
    functional::generated_artifact(
        element,
        &[],
        &FunctionalSummary {
            job: format!("{} behavior", element.name),
            source_interface: "fn sample()".into(),
            receives: vec!["input".into()],
            outcome: "returns a result".into(),
            effects: vec![],
        },
    )
}

fn dispatch(
    host: &mut SchedulingFakeHost,
    capability: &str,
    input: Value,
) -> Result<Value, PluginError> {
    KnowledgePlugin::default().dispatch(capability, input, &mut PluginContext::for_test(host))
}

fn c4_request() -> Value {
    json!({"project_root":"/repo","target_id":"folder:crates"})
}

fn change_request(element_id: &str) -> Value {
    json!({"project_root":"/repo","base_revision":1,"target_revision":2,
        "changed":[{"entity_id":element_id,"entity_kind":"parameter","disposition":"upserted"}]})
}

#[test]
fn explicit_c4_submits_only_consumed_component_summaries_and_reports_that_workload() {
    let (elements, relationships) = fixture();
    let mut host = SchedulingFakeHost {
        elements,
        relationships,
        ..Default::default()
    };

    let result = dispatch(&mut host, ENSURE_C4_EXPORT_ID, c4_request()).unwrap();

    assert_eq!(host.submitted, ["file:crates/demo.ts"]);
    assert_eq!(result["status"], "pending");
    assert_eq!(result["missing_element_ids"], json!(host.submitted));
    assert_eq!(result["pending_element_ids"], json!(host.submitted));
}

#[test]
fn change_trigger_refresh_submits_only_missing_summaries_consumed_by_saved_c4() {
    let (elements, relationships) = fixture();
    let target = elements[0].clone();
    let report = crate::c4_report::report("/repo", &target, &elements, &relationships, &[], "old");
    let mut host = SchedulingFakeHost {
        elements,
        relationships,
        artifacts: vec![report],
        ..Default::default()
    };

    dispatch(
        &mut host,
        ELEMENT_TRIGGER_EXPORT_ID,
        change_request("parameter:crates/demo.ts:render:props"),
    )
    .unwrap();

    assert_eq!(
        host.submitted,
        ["file:crates/demo.ts", "file:shared/paint.ts"]
    );
}

#[test]
fn change_trigger_refreshes_existing_stale_summary_without_a_c4_report() {
    let (elements, relationships) = fixture();
    let parameter = elements
        .iter()
        .find(|item| item.element_kind == "parameter")
        .unwrap()
        .clone();
    let mut old_parameter = parameter.clone();
    old_parameter.content_fingerprint = Some("fp1:0000000000000002:old".into());
    let mut host = SchedulingFakeHost {
        elements,
        relationships,
        artifacts: vec![functional_artifact(&old_parameter)],
        ..Default::default()
    };

    dispatch(
        &mut host,
        ELEMENT_TRIGGER_EXPORT_ID,
        change_request(&parameter.semantic_element_id),
    )
    .unwrap();

    assert_eq!(host.submitted, [parameter.semantic_element_id]);
}

#[test]
fn current_existing_summary_is_not_resubmitted_and_unrelated_change_is_not_generated() {
    let (elements, relationships) = fixture();
    let parameter = elements
        .iter()
        .find(|item| item.element_kind == "parameter")
        .unwrap()
        .clone();
    let current = functional_artifact(&parameter);
    let mut host = SchedulingFakeHost {
        elements,
        relationships,
        artifacts: vec![current],
        ..Default::default()
    };

    dispatch(
        &mut host,
        ELEMENT_TRIGGER_EXPORT_ID,
        change_request(&parameter.semantic_element_id),
    )
    .unwrap();

    assert!(host.submitted.is_empty());
}

#[test]
fn unrelated_ungenerated_change_skips_job_and_failure_reads() {
    let (elements, relationships) = fixture();
    let parameter = elements
        .iter()
        .find(|item| item.element_kind == "parameter")
        .unwrap()
        .clone();
    let mut host = SchedulingFakeHost {
        elements,
        relationships,
        ..Default::default()
    };

    dispatch(
        &mut host,
        ELEMENT_TRIGGER_EXPORT_ID,
        change_request(&parameter.semantic_element_id),
    )
    .unwrap();

    assert!(host.submitted.is_empty());
    assert!(!host.listed_tables.iter().any(|table| {
        table == "knowledge_functional_artifact_jobs"
            || table == "knowledge_functional_artifact_failures"
    }));
}

#[test]
fn explicit_c4_failure_threshold_uses_only_generation_candidates() {
    let (elements, relationships) = fixture();
    let mut host = SchedulingFakeHost {
        elements,
        relationships,
        rejected: HashSet::from(["file:crates/demo.ts".into(), "file:shared/paint.ts".into()]),
        ..Default::default()
    };

    let result = dispatch(&mut host, ENSURE_C4_EXPORT_ID, c4_request()).unwrap();

    assert_eq!(result["status"], "not_ready");
    assert_eq!(
        result["reason_code"],
        "submit_host_capability_execution_failed"
    );
}
