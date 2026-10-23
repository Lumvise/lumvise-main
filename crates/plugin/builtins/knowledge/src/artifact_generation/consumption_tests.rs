use super::*;
use crate::documents::test_graph::FakeDocumentGraph;
use crate::{KnowledgePlugin, functional, manifest::ELEMENT_TRIGGER_EXPORT_ID, semantic_context};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
struct ConsumptionFakeHost {
    rows: BTreeMap<(String, String), Value>,
    elements: Vec<SemanticElement>,
    relationships: Vec<semantic_context::SemanticRelationship>,
    graph: FakeDocumentGraph,
    statuses: HashMap<String, Value>,
    listed_tables: Vec<String>,
    selective_roots: Vec<String>,
    project_snapshots: usize,
    fail_report_lookup: bool,
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
                let elements = self
                    .elements
                    .iter()
                    .map(|element| serde_json::to_value(element).unwrap())
                    .collect::<Vec<_>>();
                let relationships = serde_json::to_value(&self.relationships).unwrap();
                Ok(self.graph.snapshot_with_sources(
                    &input,
                    &elements,
                    relationships.as_array().unwrap(),
                ))
            }
            ("storage.semantic", "selective_subgraph") => self.selective_subgraph(&input),
            ("storage.semantic", "elements_by_ids_including_inactive") => {
                let mut response = self.elements_by_ids(&input);
                let documents = self.graph.host_call(capability, input)?;
                response["elements"]
                    .as_array_mut()
                    .unwrap()
                    .extend(documents["elements"].as_array().unwrap().iter().cloned());
                Ok(response)
            }
            ("storage.semantic", "scoped_read")
                if self.fail_report_lookup
                    && input["request"]["PathPrefix"].is_object()
                    && input["request"]["PathPrefix"]["include_inactive"] != true =>
            {
                Err(PluginError::new(
                    "injected_refresh_failure",
                    "report refresh failed",
                    false,
                ))
            }
            ("storage.semantic", "candidate_source_elements") => Ok(json!({"elements": []})),
            _ => self.graph.host_call(capability, input),
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
        // Like the owner: endpoints of the kept edges outside the subtree travel as external elements.
        let external_elements: Vec<&SemanticElement> = self
            .elements
            .iter()
            .filter(|element| {
                !ids.contains(element.semantic_element_id.as_str())
                    && relationships.iter().any(|edge| {
                        edge.source_element_id == element.semantic_element_id
                            || edge.target_element_id == element.semantic_element_id
                    })
            })
            .collect();
        let subgraph = json!({"commit_version":1,"published_at":"test",
            "project_root":"/repo","root_element_id":root_id,"elements":elements,
            "relationships":relationships,"artifacts":[],"external_elements":external_elements});
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
        seed_document(&mut host.graph, external_summary.clone());
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
        seed_document(&mut host.graph, stale_report);
        host.graph.published_document_ids.clear();
    }
    host
}

fn functional_artifact(element: &SemanticElement) -> KnowledgeDocument {
    functional::generated_document(
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

fn seed_document(graph: &mut FakeDocumentGraph, document: KnowledgeDocument) {
    let source = functional::source_element_id(&document)
        .or_else(|| document.metadata["nucleus"]["target_element_id"].as_str())
        .unwrap()
        .to_owned();
    let generator = if document.document_kind == lumvise_contracts::KnowledgeDocumentKind::Report {
        documents::DocumentGenerator::C4Report
    } else {
        documents::DocumentGenerator::FunctionalContext
    };
    documents::save_generated(
        &mut PluginContext::for_test(graph),
        &generator.owner(&source),
        document,
        None,
    )
    .unwrap();
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

/// Canonical document reads for assertions: the owner decodes locator and body together.
fn document(graph: &mut FakeDocumentGraph, document_id: &str) -> Value {
    graph
        .call(
            "get_document",
            json!({"document_id": document_id, "project_root": "/repo"}),
        )
        .expect("document is readable through its owner")["document"]
        .clone()
}

fn documents_total(graph: &mut FakeDocumentGraph) -> u64 {
    graph
        .call("list_documents", json!({"project_root": "/repo"}))
        .expect("documents list through their owner")["total"]
        .as_u64()
        .unwrap()
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
    assert_eq!(documents_total(&mut host.graph), 4);
    assert_eq!(
        host.graph
            .published_document_ids
            .iter()
            .filter(|id| id.starts_with("knowledge-cultivation-report-c4-architecture-scoped-"))
            .count(),
        1
    );
    assert_eq!(
        document(
            &mut host.graph,
            "knowledge-cultivation-functional-file-src-a-rs"
        )["metadata"]["provenance"]["relationship_count"],
        2
    );
}

#[test]
fn report_refresh_failure_keeps_every_successful_job_for_retry() {
    let mut host = fixture(false);
    seed_two_successes(&mut host);
    host.fail_report_lookup = true;

    let error = dispatch_poll(&mut host).expect_err("refresh failure propagates");

    assert_eq!(error.code, "injected_refresh_failure");
    assert_eq!(documents_total(&mut host.graph), 2);
    assert_eq!(host.rows_in(JOBS), 2);

    host.fail_report_lookup = false;
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
    assert_eq!(
        document(
            &mut host.graph,
            "knowledge-cultivation-functional-file-src-a-rs"
        )["document_kind"],
        "context"
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
    let artifact = functional::generated_document(
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
    seed_document(&mut host.graph, artifact);

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
