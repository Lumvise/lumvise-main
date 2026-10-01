use super::*;
use crate::{KnowledgePlugin, functional, manifest::ELEMENT_TRIGGER_EXPORT_ID, semantic_context};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct ConsumptionFakeHost {
    rows: BTreeMap<(String, String), Value>,
    elements: Vec<SemanticElement>,
    relationships: Vec<semantic_context::SemanticRelationship>,
    artifacts: HashMap<String, Value>,
    statuses: HashMap<String, Value>,
    listed_tables: Vec<String>,
    selective_roots: Vec<String>,
    project_snapshots: usize,
    upserted_artifact_ids: Vec<String>,
    fail_artifact_lookup: bool,
}

impl HostCallTransport for ConsumptionFakeHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        match (capability, input["operation"].as_str().unwrap_or_default()) {
            ("storage.plugin", "ensure_table") => Ok(json!({})),
            ("storage.plugin", "list_rows") => self.list_rows(&input),
            ("storage.plugin", "put_row") => {
                self.rows.insert(
                    (
                        input["table_name"].as_str().unwrap_or_default().to_owned(),
                        input["row_key"].as_str().unwrap_or_default().to_owned(),
                    ),
                    input["value"].clone(),
                );
                Ok(json!({}))
            }
            ("storage.plugin", "trim_rows_by_key") => Ok(json!({})),
            ("storage.plugin", "mutate_rows") => {
                self.mutate_rows(&input);
                Ok(json!({}))
            }
            ("runtime.project_execution", "status") => self.job_status(&input),
            ("storage.semantic", "project_snapshot") => {
                self.project_snapshots += 1;
                Ok(
                    json!({"commit_version":1,"published_at":"test","project_root":"/repo",
                    "elements":self.elements,"relationships":self.relationships,
                    "artifacts":self.artifacts.values().collect::<Vec<_>>() }),
                )
            }
            ("storage.semantic", "selective_subgraph") => self.selective_subgraph(&input),
            ("storage.semantic", "elements_by_ids_including_inactive") => {
                Ok(self.elements_by_ids(&input))
            }
            ("storage.semantic", "artifacts_for_elements") => self.artifacts_for_elements(&input),
            ("storage.semantic", "upsert_artifact") => {
                let mut artifact = input["artifact"].clone();
                artifact["project_root"] = json!("/repo");
                self.upserted_artifact_ids.push(
                    artifact["artifact_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
                self.artifacts.insert(
                    artifact["artifact_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    artifact,
                );
                Ok(json!({}))
            }
            ("storage.semantic", "remove_artifact") => {
                self.artifacts
                    .remove(input["artifact_id"].as_str().unwrap_or_default());
                Ok(json!({"removed": true}))
            }
            ("storage.semantic", "candidate_source_elements") => Ok(json!({"elements": []})),
            (other, operation) => Err(PluginError::unknown_capability(&format!(
                "{other}.{operation}"
            ))),
        }
    }
}

impl ConsumptionFakeHost {
    fn rows_in(&self, table: &str) -> usize {
        self.rows
            .keys()
            .filter(|(stored_table, _)| stored_table == table)
            .count()
    }

    fn list_rows(&mut self, input: &Value) -> Result<Value, PluginError> {
        let table = input["table_name"].as_str().unwrap_or_default();
        self.listed_tables.push(table.to_owned());
        let prefix = input["key_prefix"].as_str();
        let rows = self
            .rows
            .iter()
            .filter(|((stored_table, key), _)| {
                stored_table == table && prefix.is_none_or(|prefix| key.starts_with(prefix))
            })
            .map(|(_, value)| json!({"value": value}))
            .collect::<Vec<_>>();
        Ok(json!({"rows": rows, "next_after_key": null}))
    }

    fn mutate_rows(&mut self, input: &Value) {
        for mutation in input["mutations"].as_array().into_iter().flatten() {
            if mutation["operation"] == "delete" {
                self.rows.remove(&(
                    mutation["table_name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    mutation["row_key"].as_str().unwrap_or_default().to_owned(),
                ));
            }
        }
    }

    fn job_status(&self, input: &Value) -> Result<Value, PluginError> {
        self.statuses
            .get(input["job_id"].as_str().unwrap_or_default())
            .cloned()
            .ok_or_else(|| PluginError::new("missing_status", "expected seeded job status", false))
    }

    fn selective_subgraph(&mut self, input: &Value) -> Result<Value, PluginError> {
        let root_id = input["root_element_id"].as_str().unwrap_or_default();
        self.selective_roots.push(root_id.to_owned());
        let Some(root) = self
            .elements
            .iter()
            .find(|element| element.semantic_element_id == root_id)
        else {
            return Ok(json!({"subgraph": null}));
        };
        if root.lifecycle != "active" {
            return Ok(json!({"subgraph": null}));
        }
        let elements = if root.element_kind == "folder" {
            self.elements.clone()
        } else {
            vec![root.clone()]
        };
        let ids = elements
            .iter()
            .map(|element| element.semantic_element_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let relationships = self
            .relationships
            .iter()
            .filter(|relationship| {
                ids.contains(relationship.source_element_id.as_str())
                    || ids.contains(relationship.target_element_id.as_str())
            })
            .collect::<Vec<_>>();
        let artifacts = self
            .artifacts
            .values()
            .filter(|artifact| {
                ids.contains(artifact["semantic_element_id"].as_str().unwrap_or_default())
            })
            .cloned()
            .collect::<Vec<_>>();
        let subgraph = json!({"commit_version":1,"published_at":"test",
            "project_root":"/repo","root_element_id":root_id,"elements":elements,
            "relationships":relationships,"artifacts":artifacts,"external_elements":[]});
        Ok(json!({"subgraph": subgraph}))
    }

    fn elements_by_ids(&self, input: &Value) -> Value {
        let ids = input["semantic_element_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<std::collections::HashSet<_>>();
        json!({"elements":self.elements.iter()
            .filter(|element| ids.contains(element.semantic_element_id.as_str()))
            .collect::<Vec<_>>()})
    }

    fn artifacts_for_elements(&mut self, input: &Value) -> Result<Value, PluginError> {
        if self.fail_artifact_lookup {
            return Err(PluginError::new(
                "injected_refresh_failure",
                "report refresh failed",
                false,
            ));
        }
        let ids = input["semantic_element_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<std::collections::HashSet<_>>();
        let artifacts = self
            .artifacts
            .values()
            .filter(|artifact| {
                ids.contains(artifact["semantic_element_id"].as_str().unwrap_or_default())
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(json!({"artifacts": artifacts}))
    }
}

fn element(id: &str, kind: &str, name: &str, parent: Option<&str>) -> SemanticElement {
    SemanticElement {
        project_root: "/repo".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source".into(),
        path: id.replace(':', "/"),
        element_kind: kind.into(),
        name: name.into(),
        parent_element_id: parent.map(str::to_owned),
        content_fingerprint: Some("fp-current".into()),
        start_line: Some(1),
        end_line: Some(8),
        lifecycle: "active".into(),
        metadata: json!({}),
    }
}

fn generated_output(element_id: &str, fingerprint: &str) -> Value {
    json!({"status":"succeeded","error":null,"output":{
        "semantic_element_id":element_id,"content_fingerprint":fingerprint,
        "job":"does work","source_interface":"fn run()","receives":[],
        "outcome":"returns result","effects":[]}})
}

fn pending_job(element_id: &str, job_id: &str, fingerprint: &str) -> PendingArtifactJob {
    PendingArtifactJob {
        job_id: job_id.into(),
        project_root: "/repo".into(),
        semantic_element_id: element_id.into(),
        content_fingerprint: fingerprint.into(),
        priority: SubmitPriority::Background,
        lost_resubmits: 0,
    }
}

fn seed_job(host: &mut ConsumptionFakeHost, job: PendingArtifactJob, status: Value) {
    host.rows.insert(
        (JOBS.into(), pending_key(&job)),
        serde_json::to_value(&job).expect("pending job serializes"),
    );
    host.statuses.insert(job.job_id.clone(), status);
}

fn fixture(include_report: bool) -> ConsumptionFakeHost {
    let root = element("folder:src", "folder", "src", None);
    let first = element(
        "file:src/a.rs",
        "file",
        "a.rs",
        Some(&root.semantic_element_id),
    );
    let second = element(
        "file:src/z.rs",
        "file",
        "z.rs",
        Some(&root.semantic_element_id),
    );
    let external = element("function:outside", "function", "outside", None);
    let relationships = vec![
        semantic_context::SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: first.semantic_element_id.clone(),
            target_element_id: external.semantic_element_id.clone(),
            relationship_kind: "semantic".into(),
            label: "calls".into(),
            lifecycle: "active".into(),
            metadata: json!({}),
        },
        semantic_context::SemanticRelationship {
            project_root: "/repo".into(),
            source_element_id: external.semantic_element_id.clone(),
            target_element_id: first.semantic_element_id.clone(),
            relationship_kind: "semantic".into(),
            label: "called_by".into(),
            lifecycle: "active".into(),
            metadata: json!({}),
        },
    ];
    let mut host = ConsumptionFakeHost {
        elements: vec![root.clone(), first, second, external],
        relationships: relationships.clone(),
        ..Default::default()
    };
    if include_report {
        let external_summary = functional_artifact(&host.elements[3]);
        host.artifacts.insert(
            external_summary.artifact_id.clone(),
            graph_artifact(&external_summary),
        );
        let report = crate::c4_report::report(
            "/repo",
            &root,
            &host.elements,
            &relationships,
            &[external_summary],
            "old-fingerprint",
        );
        let mut stale_report = report;
        stale_report.metadata["nucleus"]["dependency_fingerprint"] = json!("old-fingerprint");
        let encoded = graph_artifact(&stale_report);
        host.artifacts.insert(stale_report.artifact_id, encoded);
    }
    host
}

fn functional_artifact(element: &SemanticElement) -> crate::KnowledgeArtifact {
    functional::generated_artifact(
        element,
        &[],
        &functional::FunctionalSummary {
            job: "does work".into(),
            source_interface: "fn run()".into(),
            receives: vec![],
            outcome: "returns result".into(),
            effects: vec![],
        },
    )
}

fn graph_artifact(artifact: &crate::KnowledgeArtifact) -> Value {
    let encoded = serde_json::to_value(artifact).expect("knowledge artifact serializes");
    json!({"project_root":"/repo","artifact_id":encoded["artifact_id"],
        "semantic_element_id":encoded["semantic_element_id"],
        "artifact_kind":encoded["knowledge_type"],"title":encoded["title"],
        "content_ref":null,"content":encoded["content"],"searchable_text":null,
        "dependencies":encoded["dependencies"],"content_size_bytes":null,
        "metadata":{"knowledge":{"tags":encoded["tags"],"metadata":encoded["metadata"],
            "path":encoded["path"],"project_root":encoded["project_root"]}}})
}

fn dispatch_poll(host: &mut ConsumptionFakeHost) -> Result<Value, PluginError> {
    KnowledgePlugin::default().dispatch(
        crate::ARTIFACT_GENERATION_POLL_EXPORT_ID,
        json!({}),
        &mut PluginContext::for_test(host),
    )
}

fn dispatch_storage_change(host: &mut ConsumptionFakeHost, element_id: &str) {
    KnowledgePlugin::default()
        .dispatch(
            ELEMENT_TRIGGER_EXPORT_ID,
            json!({"project_root":"/repo","base_revision":1,"target_revision":2,
                "changed":[{"entity_id":element_id,"entity_kind":"file","disposition":"upserted"}]}),
            &mut PluginContext::for_test(host),
        )
        .expect("storage change dispatch succeeds");
}

fn seed_two_successes(host: &mut ConsumptionFakeHost) {
    for (element_id, job_id) in [("file:src/a.rs", "job-a"), ("file:src/z.rs", "job-z")] {
        seed_job(
            host,
            pending_job(element_id, job_id, "fp-current"),
            generated_output(element_id, "fp-current"),
        );
    }
}

#[test]
fn poll_batches_shared_report_refresh_and_stores_with_selective_subgraphs() {
    let mut host = fixture(true);
    seed_two_successes(&mut host);

    let response = dispatch_poll(&mut host).expect("poll succeeds");

    assert_eq!(response["completed"], 2);
    assert_eq!(response["pending"], 0);
    assert_eq!(
        host.selective_roots
            .iter()
            .filter(|root| *root == "folder:src")
            .count(),
        1
    );
    assert_eq!(host.project_snapshots, 0);
    assert_eq!(host.artifacts.len(), 4);
    assert_eq!(
        host.upserted_artifact_ids
            .iter()
            .filter(|id| id.starts_with("knowledge-cultivation-report-c4-architecture-scoped-"))
            .count(),
        1
    );
    assert_eq!(
        host.artifacts["knowledge-cultivation-functional-file-src-a-rs"]["metadata"]["knowledge"]["metadata"]
            ["provenance"]["relationship_count"],
        2
    );
}

#[test]
fn report_refresh_failure_keeps_every_successful_job_for_retry() {
    let mut host = fixture(false);
    seed_two_successes(&mut host);
    host.fail_artifact_lookup = true;

    let error = dispatch_poll(&mut host).expect_err("refresh failure propagates");

    assert_eq!(error.code, "injected_refresh_failure");
    assert_eq!(host.artifacts.len(), 2);
    assert_eq!(host.rows_in(JOBS), 2);

    host.fail_artifact_lookup = false;
    let response = dispatch_poll(&mut host).expect("retry completes stored jobs");

    assert_eq!(response["completed"], 2);
    assert_eq!(response["pending"], 0);
}

#[test]
fn later_stale_job_does_not_discard_earlier_successful_consumption() {
    let mut host = fixture(true);
    let first = pending_job("file:src/a.rs", "job-a", "fp-current");
    let stale = pending_job("file:src/z.rs", "job-z", "fp-old");
    seed_job(
        &mut host,
        first.clone(),
        generated_output("file:src/a.rs", "fp-current"),
    );
    seed_job(
        &mut host,
        stale.clone(),
        generated_output("file:src/z.rs", "fp-current"),
    );

    let error = dispatch_poll(&mut host).expect_err("stale later output propagates");

    assert_eq!(error.code, "stale_project_execution_output");
    assert!(!host.rows.contains_key(&(JOBS.into(), pending_key(&first))));
    assert!(host.rows.contains_key(&(JOBS.into(), pending_key(&stale))));
    assert!(
        host.artifacts
            .contains_key("knowledge-cultivation-functional-file-src-a-rs")
    );
    assert_eq!(
        host.selective_roots
            .iter()
            .filter(|root| *root == "folder:src")
            .count(),
        1
    );
}

#[test]
fn current_summary_change_skips_job_and_failure_table_reads() {
    let mut host = fixture(false);
    let element = host.elements[1].clone();
    let artifact = functional::generated_artifact(
        &element,
        &[],
        &functional::FunctionalSummary {
            job: "does work".into(),
            source_interface: "fn run()".into(),
            receives: vec![],
            outcome: "returns result".into(),
            effects: vec![],
        },
    );
    host.artifacts
        .insert(artifact.artifact_id.clone(), graph_artifact(&artifact));

    dispatch_storage_change(&mut host, &element.semantic_element_id);

    assert!(
        !host
            .listed_tables
            .iter()
            .any(|table| table == JOBS || table == FAILURES)
    );
}

#[test]
fn ungenerated_change_without_a_cultivation_candidate_skips_job_tables() {
    let mut host = fixture(false);
    let element_id = host.elements[1].semantic_element_id.clone();

    dispatch_storage_change(&mut host, &element_id);

    assert!(
        !host
            .listed_tables
            .iter()
            .any(|table| table == JOBS || table == FAILURES)
    );
}

#[test]
fn selective_store_rejects_missing_inactive_and_stale_targets() {
    for state in ["missing", "inactive", "stale"] {
        let mut host = fixture(false);
        if state == "missing" {
            host.elements
                .retain(|item| item.semantic_element_id != "file:src/a.rs");
        } else if state == "inactive" {
            host.elements
                .iter_mut()
                .find(|item| item.semantic_element_id == "file:src/a.rs")
                .unwrap()
                .lifecycle = "inactive".into();
        }
        let fingerprint = if state == "stale" {
            "fp-old"
        } else {
            "fp-current"
        };
        let pending = pending_job("file:src/a.rs", "job-a", fingerprint);
        seed_job(
            &mut host,
            pending.clone(),
            generated_output("file:src/a.rs", fingerprint),
        );

        let error = dispatch_poll(&mut host).expect_err("invalid target rejects output");

        assert_eq!(
            error.code, "stale_project_execution_output",
            "case: {state}"
        );
        assert!(
            host.rows
                .contains_key(&(JOBS.into(), pending_key(&pending)))
        );
    }
}
